//! Finds and checks the player's own STRAFTAT installation. Nothing is copied,
//! exported or downloaded: Restrafe reads `STRAFTAT_Data/data.unity3d` in place.
//!
//!   cargo run --release --bin setup -- [--game <STRAFTAT install folder>]
//!
//! Order: `--game` > `RESTRAFE_ROOT` > remembered folder > Steam libraries. A folder given with `--game` is
//! remembered once it passes the check (`%LOCALAPPDATA%\restrafe\installation.json`).

#[path = "../install.rs"]
mod install;
#[path = "../dotnet_meta.rs"]
#[allow(dead_code)]
mod dotnet_meta;
#[path = "../unity_fs.rs"]
#[allow(dead_code)]
mod unity_fs;

use std::path::PathBuf;

fn fail(msg: &str) -> ! {
    eprintln!("setup error: {msg}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let explicit = args.iter().position(|a| a == "--game").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let remember = explicit.is_some();
    let root = install::discover(explicit).unwrap_or_else(|e| fail(&e));
    let build = install::installed_build(&root);
    println!("STRAFTAT: {} (Steam build {})", root.display(), build.as_deref().unwrap_or("unknown"));
    if build.as_deref().is_some_and(|b| b != install::TESTED_BUILD) {
        println!("Note: Restrafe was made against build {}; other builds are unverified.", install::TESTED_BUILD);
    }
    // Representative content check (not a full integrity scan): the bundle opens and every serialized file parses.
    let bundle = unity_fs::Bundle::open(&root.join(install::DATA_DIR).join(install::BUNDLE)).unwrap_or_else(|e| fail(&e));
    let (mut files, mut objects, mut bad) = (0, 0, Vec::new());
    for n in bundle.nodes.iter().filter(|n| !n.path.ends_with(".resS") && !n.path.ends_with(".resource")) {
        match unity_fs::SerializedFile::open(&bundle, n) {
            Ok(sf) => {
                files += 1;
                objects += sf.objects.len();
            }
            Err(e) => bad.push(format!("{}: {e}", n.path)),
        }
    }
    if !bad.is_empty() {
        fail(&format!("{} unreadable files (verify the game files in Steam):\n{}", bad.len(), bad.join("\n")));
    }
    if remember {
        install::remember(&root);
    }
    println!("OK: Unity {}, {files} serialized files, {objects} objects readable in place.", bundle.unity_version);
}
