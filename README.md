# Restrafe

[![CI](https://github.com/notcatnine/restrafe/actions/workflows/ci.yml/badge.svg)](https://github.com/notcatnine/restrafe/actions/workflows/ci.yml)

An experimental, single-player Rust/[Bevy](https://bevyengine.org) 0.19 reconstruction of STRAFTAT's movement,
weapons, HUD, health, player model, cosmetics, audio and settings, played in one test map with a training dummy.
It reads the models, textures, sounds, animations and data of **your own installed copy of STRAFTAT** in place. It
does not load the game's executable or its Unity/managed DLLs, and it never exports or copies game files.
Multiplayer, the other maps, music and the main menu are out of scope.

**This repository contains no STRAFTAT files.** You must own STRAFTAT on Steam. Restrafe is not affiliated with or
endorsed by the developers or publisher of STRAFTAT.

## Setup steps

1. **Install STRAFTAT through Steam.** Restrafe needs the files of your owned Steam PC copy. The repository does
   not include or download them.
2. **Install the build tools.** This repository contains source, not a ready-to-play executable. On Windows,
   install [Rust](https://rustup.rs/) with the MSVC toolchain, [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/)
   with **Desktop development with C++** and a Windows SDK, and [Git](https://git-scm.com/downloads).
3. **Build and play.** In PowerShell or a command prompt:

   ```powershell
   git clone https://github.com/notcatnine/restrafe.git
   cd restrafe
   .\launch.cmd
   ```

   `launch.cmd` builds Restrafe, runs `setup` the first time, puts **`Restrafe.exe`** in the `restrafe` folder and
   starts the game. Rust is only needed to build and rebuild.
4. **Play.** From then on, just double-click **`Restrafe.exe`** in the `restrafe` folder; no command prompt
   needed. For a desktop or Start menu shortcut, right-click it > Show more options > Send to > Desktop (create
   shortcut), or Pin to Start. After pulling new code, run `launch.cmd` again to rebuild it. If STRAFTAT cannot be
   found, Restrafe says so in a message box.

**The first launch takes a little longer.** The first build compiles everything (several minutes). The first time
the game starts, it also indexes the names of the objects in your installation (about 10-15 seconds), then caches
that index in `%LOCALAPPDATA%\restrafe\` so later launches only take a few seconds. The window can stay dark while
this happens. A STRAFTAT update rebuilds the index once.

`setup` finds STRAFTAT in this order: `--game <folder>`, then the `RESTRAFE_ROOT` environment
variable, then the remembered folder (`%LOCALAPPDATA%\restrafe\installation.json`), then your Steam libraries. To find
the folder yourself, open Steam > STRAFTAT > Properties > Installed Files > Browse. It checks that
`STRAFTAT_Data/data.unity3d` can be read and remembers the folder. Nothing is copied, exported or downloaded.

To build by hand instead, from `game/`: `cargo build --release --bins`, `cargo run --release --bin setup`, then
`cargo run --release`.

## Controls

Click the window to capture the mouse.

| Action | Key |
| --- | --- |
| Move / jump | WASD / Space |
| Sprint | Shift |
| Crouch, slide (while sprinting) | C or Ctrl |
| Lean left / right | Q / E |
| Pick up / drop a weapon | F |
| Fire the right / left hand | Left / right mouse button |
| Aim | Right mouse button |
| Swap hands | Mouse wheel |
| Voice lines | 1-0 |
| Settings and pause menu | Escape |
| **Weapons menu** | **` (backquote, the key to the right of backspace)** |
| **Cosmetics menu** | **K** |

**Weapons menu (`, the key to the right of Backspace).** This one isn't in the original game. It replaces finding weapons on the map: it lists every
weapon by its in-game name. Click one to spawn it in front of you, and it is picked up the normal way (into a free
hand). Press ` again or Escape to close it. It can't be opened while you are dead or while another menu is open.

**Cosmetics menu (K).** The game's own customization screen. Pick a hat, suit or smoke from the **Hats**,
**Suits** and **Smoke** tabs; the 3D preview on the right and your player model change straight away. Close it with
**OK**, K or Escape. Everything is unlocked, and your choice is saved to `game/config/cosmetics.ron`.

Settings are saved to `game/config/settings.ron`.

## Platforms and tested build

| System | Status |
| --- | --- |
| Windows 11 64-bit (x86_64) | Primary tested platform; CI builds and tests every push |
| macOS, Linux | **Not tested** |

The tested installation is the Steam build `25333005` of STRAFTAT, with Rust `1.99.0`. Other game builds are
unverified: `setup` reports the installed build and warns when it differs.

## How the game is read

Restrafe reads the installed `STRAFTAT_Data/data.unity3d` directly:

- **Bundle and serialized files**: UnityFS with LZ4 blocks, Unity 2021.3 player object layouts, Unity's
  `unity default resources` / `unity_builtin_extra`.
- **Textures**: BC1-7 and uncompressed formats, uploaded as stored or decoded where the CPU needs pixels; normal maps
  unpacked like Unity's `UnpackNormal`.
- **Audio**: FSB5 Vorbis, decoded in memory; the Vorbis setup headers the banks leave out come from the game's own
  player DLL table or are regenerated with upstream libvorbis.
- **Models**: prefabs, meshes, skinning and the training map, with lightmaps, reflection probes, light probes and sky.
- **Scripts and components**: the managed assemblies' metadata (ECMA-335) gives every MonoBehaviour's serialized
  layout; engine components use Unity 2021.3 type trees.
- **Animation**: packed clips (streamed Hermite segments, dense samples, constants) rebuilt into curves; animator
  controllers from their baked state machines; the humanoid Avatar.
- **UI**: the menu scene and prefabs, sprites, materials and TextMeshPro fonts.

Details, decisions and the stages are in [docs/in-place-reading.md](docs/in-place-reading.md).
`cargo run --release --bin inspect` lists what can be read.

## Implemented so far

Movement (strafing, sliding, wall jumps, crouch, lean), every weapon with its mechanics, projectiles and effects, the
3D HUD and health, the player rig with humanoid animation and first-person arms, all hats / suits / cigarettes,
ragdolls and props, sound effects and voice lines, and the settings, pause and customization menus, each checked
against the original's code and data.

## Validation

Two kinds of checks, described in [docs/validation.md](docs/validation.md):

- **Reference tests** (`cargo test --release`, all of them with `RESTRAFE_REF_CHECKS=1`): what is read in place is
  compared with reference data, e.g. 2,836 prefab / scene objects (129,288 fields), 149 animation clips at every key,
  71 animator controllers, 1,505 materials, 23 fonts, 68 normal maps. Without an installation they are skipped.
- **Autotests** (`RESTRAFE_AUTOTEST=<name>`): scripted play sessions that log results and save screenshots, e.g.
  the Glock kills the dummy on the 4th shot, the map builds its 170 colliders, the menus render.

## Fidelity work remaining

- One humanoid pose (`Aboubi_Gun_TwoHanded`) misses its stored hand IK goals by about 5 cm: its shoulder muscles are
  far outside their range. Every other clip is within 1.4 cm.
- macOS and Linux have never been run.
- Only one STRAFTAT build has been tested.

Report other differences from the original as GitHub issues.

## Repository layout

- `game/src/`: the game (`restrafe`). `install.rs` finds the installation; `unity_fs.rs`, `owned*.rs`,
  `prefab_json.rs`, `dotnet_meta.rs`, `anim_clip.rs` and `anim_ctl.rs` read it in place.
- `game/src/bin/`: `setup` (find and check the installation) and `inspect` (list what can be read).
- `game/assets/shaders/`: Restrafe's own WGSL shaders, the only files under `game/assets/`.
- `game/vendor/bevy_pbr/`: Bevy's `bevy_pbr` crate (MIT OR Apache-2.0, license files included) with two changes:
  the lightmap slab size is 32 instead of 4, and deferred lightmapped meshes are not batched.
- `game/vendor/libvorbis/`, `game/vendor/libogg/`: upstream Xiph libvorbis 1.3.4 and libogg (BSD-3-Clause,
  `COPYING` included), built by `game/build.rs`. Used only to regenerate Vorbis setup headers; see `game/src/fsb5.rs`.
- `docs/`: how the installation is read, and how the port is validated.

## Research boundary

The port follows the original game's own data and code. The reading formats come from public specifications
(ECMA-335, the Unity serialized-file and UnityFS layouts as documented by open-source tools such as UnityPy) and from
the files of an owned installation. Nothing derived from the game (decompiled code, extracted data, captures) is in
this repository.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Restrafe's own source code is released under the [MIT License](LICENSE). Third-party code keeps its own license:
Cargo crates (see `game/Cargo.lock`; e.g. `lewton` and `texture2ddecoder`, MIT OR Apache-2.0), `game/vendor/bevy_pbr/`
(MIT OR Apache-2.0) and `game/vendor/libvorbis/` + `game/vendor/libogg/` (BSD-3-Clause). The Unity type tree data in
`game/src/engine_types.txt` is generated from UnityPy's type tree package (MIT). STRAFTAT and all of its content belong
to their respective owners; none of it is covered by this license.
