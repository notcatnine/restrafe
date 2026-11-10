# Contributing

Restrafe is a partial, single-player Rust reconstruction of STRAFTAT's mechanics. Please scope changes to one
concrete behaviour and describe what remains approximate.

## Build and check

Follow the setup steps in [README.md](README.md), then from `game/`:

```powershell
cargo build --release --bins --locked
cargo run --release
```

Run the checks in [docs/validation.md](docs/validation.md) that touch your change (`cargo test --release`, the
autotests). Compare against the installed game, and say in the pull request what you tested (situation, weapon, settings, the
STRAFTAT build). Separate what you observed in the game from what you inferred.

## Code and distribution boundaries

- Ship only project source and properly attributed open-source dependencies.
- Never commit game files: no ripped models, textures, sounds, fonts, prefab or scene data, no generated
  `game/assets/` content other than `game/assets/shaders/`, no game executables or DLLs, no decompiler output,
  and no research tools or databases.
- Do not export or rip the game. Read the owned installation in place through `install.rs` / `unity_fs.rs`; new
  data types get a reader there.
- Do not commit credentials, screenshots of game content, or private local paths.
- `.gitignore` covers the generated folders. Check `git status` before committing.
