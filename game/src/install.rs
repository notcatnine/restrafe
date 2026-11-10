//! Finding the player's own STRAFTAT installation, in this order:
//! `--game <folder>` > `RESTRAFE_ROOT` > the remembered folder (`installation.json`) > Steam discovery
//! (the registry's SteamPath and the default paths, every library in `libraryfolders.vdf`, the app manifest's
//! `installdir`). A folder is valid when it holds `STRAFTAT_Data/data.unity3d`; the `STRAFTAT_Data` folder itself
//! is accepted too. The installation is only ever read.

use std::path::{Path, PathBuf};

/// Steam app id of STRAFTAT.
pub const APP_ID: u32 = 2386720;
/// The Steam build Restrafe was made against (`buildid` in the app manifest).
#[allow(dead_code)] // read by the setup / inspect tools, not by the game
pub const TESTED_BUILD: &str = "25333005";
pub const DATA_DIR: &str = "STRAFTAT_Data";
pub const BUNDLE: &str = "data.unity3d";

pub fn candidate_steam_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"C:\Program Files\Steam")];
    #[cfg(windows)]
    if let Ok(out) = std::process::Command::new("reg").args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some((_, v)) = s.lines().find(|l| l.contains("REG_SZ")).and_then(|l| l.split_once("REG_SZ")) {
            roots.insert(0, PathBuf::from(v.trim()));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        roots.push(h.join("Library/Application Support/Steam"));
        roots.push(h.join(".steam/steam"));
        roots.push(h.join(".local/share/Steam"));
    }
    roots
}

/// `"key" "value"` pairs of a Valve KeyValues text file (libraryfolders.vdf, appmanifest_*.acf), in order.
fn vdf_pairs(text: &str) -> Vec<(String, String)> {
    let mut toks = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '"' {
            let mut s = String::new();
            while let Some(c) = it.next() {
                match c {
                    '\\' => {
                        if let Some(n) = it.next() {
                            s.push(n);
                        }
                    }
                    '"' => break,
                    c => s.push(c),
                }
            }
            toks.push(Some(s));
        } else if c == '{' || c == '}' {
            toks.push(None);
        }
    }
    toks.windows(2).filter_map(|w| match (&w[0], &w[1]) {
        (Some(k), Some(v)) => Some((k.clone(), v.clone())),
        _ => None,
    }).collect()
}

fn app_manifest(library: &Path) -> Option<Vec<(String, String)>> {
    std::fs::read_to_string(library.join(format!("steamapps/appmanifest_{APP_ID}.acf"))).ok().map(|s| vdf_pairs(&s))
}

pub fn discover_steam() -> Result<PathBuf, String> {
    let roots = candidate_steam_roots();
    let mut libraries = roots.clone();
    for root in &roots {
        if let Ok(s) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libraries.extend(vdf_pairs(&s).into_iter().filter(|(k, _)| k == "path").map(|(_, v)| PathBuf::from(v)));
        }
    }
    let mut seen = Vec::new();
    for lib in libraries {
        let key = lib.to_string_lossy().to_lowercase().replace('/', "\\");
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let dir = app_manifest(&lib).and_then(|m| m.into_iter().find(|(k, _)| k == "installdir").map(|(_, v)| v)).unwrap_or_else(|| "STRAFTAT".into());
        if let Ok(p) = validate(lib.join("steamapps/common").join(dir)) {
            return Ok(p);
        }
    }
    Err("STRAFTAT not found in any Steam library; pass --game <install folder> or set RESTRAFE_ROOT".into())
}

/// Normalizes `path` to the install folder (the one containing STRAFTAT_Data) and checks the bundle is there.
pub fn validate(path: PathBuf) -> Result<PathBuf, String> {
    let path = if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(DATA_DIR)) { path.parent().map(Path::to_path_buf).unwrap_or(path) } else { path };
    if !path.join(DATA_DIR).join(BUNDLE).is_file() {
        return Err(format!("{} is not a STRAFTAT installation (missing {DATA_DIR}/{BUNDLE})", path.display()));
    }
    let p = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
    // canonicalize gives Windows verbatim paths (\\?\C:\...); keep the ordinary form.
    Ok(p.to_str().and_then(|s| s.strip_prefix(r"\\?\")).map(PathBuf::from).unwrap_or(p))
}

/// Folder of `installation.json` (`RESTRAFE_CONFIG_DIR` overrides it).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RESTRAFE_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("restrafe"))
}

pub fn remembered() -> Option<PathBuf> {
    let s = std::fs::read_to_string(config_dir()?.join("installation.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("game").and_then(|g| g.as_str()).map(PathBuf::from)
}

pub fn remember(path: &Path) {
    if let Some(dir) = config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("installation.json"), serde_json::to_string_pretty(&serde_json::json!({ "game": path })).unwrap_or_default());
    }
}

/// `explicit` (`--game`) > `RESTRAFE_ROOT` > remembered folder > Steam. Explicit overrides are not saved; a
/// folder found through Steam (or a remembered one that stopped working and was rediscovered) is remembered.
pub fn discover(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return validate(p);
    }
    if let Some(p) = std::env::var_os("RESTRAFE_ROOT") {
        return validate(PathBuf::from(p));
    }
    if let Some(p) = remembered().and_then(|p| validate(p).ok()) {
        return Ok(p);
    }
    let p = discover_steam()?;
    remember(&p);
    Ok(p)
}

/// The installed Steam build id, when the app manifest next to the install says so.
pub fn installed_build(root: &Path) -> Option<String> {
    let lib = root.parent()?.parent()?.parent()?;
    app_manifest(lib)?.into_iter().find(|(k, _)| k == "buildid").map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vdf_pairs_reads_paths_and_escapes() {
        let t = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n}";
        assert_eq!(vdf_pairs(t), vec![("path".to_string(), r"C:\Program Files (x86)\Steam".to_string())]);
    }
    #[test]
    fn invalid_root_fails_validation() {
        assert!(validate(std::env::temp_dir().join("restrafe-no-such-game")).is_err());
    }
}
