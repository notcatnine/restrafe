//! Finds and checks the player's own STRAFTAT installation. Nothing is copied,
//! exported or downloaded: Restrafe reads `STRAFTAT_Data/data.unity3d` in place.
//!
//!   setup.exe [--game <STRAFTAT install folder>] [--no-pause]
//!   (from source: cargo run --release --bin setup -- [...])
//!
//! Order: `--game` > `RESTRAFE_ROOT` > remembered folder > Steam libraries; when none is found and setup runs in a
//! window, it asks for the folder. The folder is remembered once it passes the check
//! (`%LOCALAPPDATA%\restrafe\installation.json`). Started by double-click, it waits for Enter before closing
//! (`--no-pause` skips that, as `launch.cmd` does).

#[path = "../install.rs"]
mod install;
#[path = "../dotnet_meta.rs"]
#[allow(dead_code)]
mod dotnet_meta;
#[path = "../unity_fs.rs"]
#[allow(dead_code)]
mod unity_fs;

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

/// Waits for Enter when setup runs in its own window (double-clicked), so its result stays readable.
fn pause() {
    let args: Vec<String> = std::env::args().collect();
    if std::io::stdin().is_terminal() && !args.iter().any(|a| a == "--no-pause") {
        print!("\nPress Enter to close.");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().lock().read_line(&mut String::new());
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("setup error: {msg}");
    pause();
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let explicit = args.iter().position(|a| a == "--game").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let root = match install::discover(explicit.clone()) {
        Ok(r) => r,
        // not found: ask for the folder when someone is there to answer
        Err(e) if explicit.is_none() && std::io::stdin().is_terminal() => {
            println!("{e}\n");
            println!("Paste the STRAFTAT folder (Steam > STRAFTAT > Properties > Installed Files > Browse) and press Enter:");
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            let folder = line.trim().trim_matches('"');
            if folder.is_empty() {
                fail("no folder given");
            }
            install::discover(Some(PathBuf::from(folder))).unwrap_or_else(|e| fail(&e))
        }
        Err(e) => fail(&e),
    };
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
    install::remember(&root);
    println!("OK: Unity {}, {files} serialized files, {objects} objects readable in place.", bundle.unity_version);
    println!("Restrafe will use this folder. Start the game with Restrafe.exe.");
    pause();
}
