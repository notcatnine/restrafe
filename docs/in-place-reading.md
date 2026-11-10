# Reading STRAFTAT in place

Restrafe reads the player's installed `STRAFTAT_Data/data.unity3d` directly: no export step,
no AssetRipper, no copy of the game on disk. `setup` only finds and checks the installation.

## Status (2026-11-07)

On the development machine the game runs with every exported texture, sound, model, the map, its lightmaps,
cubemaps, collision meshes and fonts removed (1.3 GB -> 34 MB of `game/assets`, all of it extracted JSON data).
Next: MonoBehaviour field layouts from the game's `Managed/*.dll` metadata (the build has no type trees), then the
prefab / material / scene JSON, animation clips and the UI scene.

## Done

- **Install discovery** (`game/src/install.rs`): `--game` > `RESTRAFE_ROOT` > remembered
  folder (`%LOCALAPPDATA%\restrafe\installation.json`, `RESTRAFE_CONFIG_DIR` overrides) > Steam (registry
  SteamPath, default paths, every `libraryfolders.vdf` library, `appmanifest_2386720.acf` installdir). The
  installed Steam build is compared with the tested one (25333005).
- **Bundle reader** (`game/src/unity_fs.rs`): UnityFS header, LZ4/LZ4HC block table and directory, random-access
  reads that decompress only the blocks covering a range, SerializedFile v22 object tables. On the tested build:
  1,493 bundle nodes, 754 serialized files, 697,704 objects, listed in ~0.1-0.3 s.
- `cargo run --release --bin inspect` prints the found install and the object counts per class.
- **First object layouts** (Unity 2021.3 player format): `AudioClip` (format, channels, rate, length, streamed
  FSB5 payload in `sharedassets*.resource`) and `Texture2D` (size, format, mips, colour space, sampler settings, pixel
  data inline or streamed from `.resS`). Checked: `inspect --audio Prophet_Loop_00` gives 2 ch / 44.1 kHz / 6.17 s
  Vorbis with an `FSB5` payload; `inspect --texture Ramp03` gives 64x64 DXT1 with 7 mips = 2,744 bytes, exactly
  the DXT1 mip-chain size, at the path id the DarkTrail material references.
- `Material` (shader, keywords, render queue, tags, textures with tiling, ints, floats, colours): `inspect --material
  DarkTrail` / `M_BlackHole_00` match the values read with UnityPy (queue 3000, `_Mode` 5, `_MainTex` -> 564 at
  scale (0.5, 1); shader 1936, `_Float0` 0.003).
- `GameObject` and `Transform` (components, layer, name, tag, active; local TRS, children, parent):
  `inspect --prefab ProphetProj` walks the prefab in place and matches UnityPy and the prefab data (root path id 4397,
  scales 0.0948 / 12 / 1.3977, inactive children, layer 13 on `PP`, component classes).
- Renderer head (MeshRenderer / SkinnedMeshRenderer / ParticleSystemRenderer: enabled, shadows, probes, lightmap,
  materials) and MeshFilter: the same walk lists every renderer's materials and enabled flag and every mesh
  reference, identical to the UnityPy reading (`ball` enabled with M_BlackHole_00, `VFX_ProphetSphere_00` disabled,
  built-in sphere mesh 10207).

## Progress

Measured against one fixed checklist of everything the game used to read from exported or ripped files, weighted by
effort.

| Item | Weight | Status |
|---|---|---|
| Install discovery + bundle / serialized-file reader | 5 | done |
| Textures | 8 | done |
| Audio | 8 | done |
| Models (prefab / mesh / map GLBs) | 12 | done |
| Lightmaps, cubemaps, fonts, collision meshes | 5 | done |
| Prefab data (weapons, effects, props, projectiles, player, hats, map) | 15 | done |
| Materials, post-processing, probes, cosmetics list, sprites, texture settings, HUD springs, skinning | 7 | done |
| Mixer gains | 2 | done |
| TMP fonts (HUD / 3D text) | 4 | done |
| Animation clips + controllers (player, arms, weapons, props) | 10 | done |
| Avatar (humanoid rig) | 4 | done |
| MainMenu UI scene + UI assets (pause / settings / cosmetics menus, world material layer) | 15 | done |
| Verification: real reference tests, game runs with `ripped/` renamed | 5 | done |

**Overall: 100%.** The game runs with the old rip (`ripped/`, `ripped_gltf/`) renamed away: every regression
passes and the menu / HUD / cosmetics screenshots match. Nothing outside `#[cfg(test)]` refers to the rip; the
tests use it (and `dev/moved-assets`) only as reference data. Every comparison test runs with `RESTRAFE_REF_CHECKS=1` (references found in `dev/moved-assets` or
the old rip): prefab / scene data (2,836 objects, 129,288 fields), 149 animation clips (5,038 curve components at
every key), 71 controllers, the Avatar, 23 TMP fonts, 1,505 material documents, 96 effect materials, 68 normal maps,
10 mixers, 1,608 texture headers, 36 skinned meshes and more.

Fixes found by these checks: Alpha8 textures decoded as (v, v, v, 255) instead of (0, 0, 0, v); normal maps passed
to the renderer still in Unity's DXT5nm layout (now unpacked like UnpackNormal); the built-in sprite ids 10911 /
10917 were swapped (input fields drew the mask sprite); RectOffset (layout padding) was not read; duplicate-name
suffixes are case-insensitive like the old exports; dense animation curves interpolate linearly (as Unity plays them)
instead of with the old export's zero tangents.

Known, unrelated to reading: the humanoid IK goal check is off by up to 10 cm on Aboubi_Gun_TwoHanded (the same with
the old YAML), every other clip within about 1 cm.

## Remaining stages (the game's systems move onto the in-place readers)

The build has no embedded type trees, so object layouts come from Unity 2021.3's engine classes and, for
MonoBehaviours, from the game's own `Managed/*.dll` metadata (read in place too).

1. Object reader: engine class layouts for the classes Restrafe uses (GameObject, Transform, MeshRenderer, Material,
   Texture2D, Mesh, AudioClip, AnimationClip, Avatar, ParticleSystem, ...), external references between files.
2. MonoBehaviour layouts from the managed DLLs' metadata (field order, alignment, nested structs, references).
   Started: `game/src/dotnet_meta.rs` reads ECMA-335 metadata in place (PE CLI header, #~ / #Strings / #Blob,
   TypeDef / Field / TypeRef tables, field signatures); `inspect --class CosmeticInstance` lists its 16 fields in
   declaration order with their types, as in the decompiled source. Custom attributes are read too
   (`FieldDef::unity_serialized`: public or [SerializeField], not static / const / readonly / [NonSerialized]); for
   CosmeticInstance it selects exactly the fields the scene data holds. First MonoBehaviour reader
   (`unity_fs::read_monobehaviour`): primitives (small ones padded to 4 bytes), strings, object references, Unity
   math structs, arrays / Lists; `inspect --mb CosmeticInstance` decodes the menu's buttons in place with exactly
   the values of the old scene export ("No Hat", flags, colours). All `Managed/*.dll` are loaded
   (`Assemblies`): inherited fields across assemblies (FishNet `NetworkBehaviour` first), enums, nested
   [Serializable] types, and Unity's serializable-type rule. The Revolver's `Gun` component (all its fields,
   inherited ones included) decodes in place with the same values as the old export (stats, clips, effects,
   layer masks). Next: AnimationCurve / Gradient / UnityEvent, then generate the prefab / scene data the game
   reads (data/weapons, projectiles, props, vfx, map) from it.
3. Textures: **in use.** `owned.rs` installs a default asset source: an image path that is not under
   `game/assets` is read from the installation by texture name (`T_Heart_01.png` -> Texture2D `T_Heart_01`) and
   loaded by `UtexLoader` in its stored GPU format with all mips and Unity's sampler settings (loader settings such
   as `is_srgb = false` still apply). Unity stores rows bottom-up: BC1/3/4/5 levels are flipped block-wise without
   decoding (bit-exact against the old exports: T_Heart_01, Ramp03, T_JumpSuit_01, T_Boubi_Face00); BC6H/BC7 are
   decoded (`texture2ddecoder`, MIT/Apache) and flipped. The VFX library (CPU pixel edits) decodes mip 0 the same
   way. The name index (3,306 textures) is cached per game build next to installation.json. Names the old
   exports changed are still found (case / accents / spaces, and the `_N` suffix of a duplicate name). Checked by
   running the game with all 140 such textures (HUD, VFX, cosmetics, weapons) removed from game/assets:
   regressions pass and the cosmetics, HUD and effects screenshots are unchanged. Still from the old export:
   the two reflection-probe cubemaps and the sky (Cubemap objects), and textures used by the UI scene importer
   and the TMP font atlas (they move with stage 6).
4. Audio: **in use.** A sound path that is not under `game/assets` is decoded in place (`owned.rs`, `fsb5.rs`):
   AudioClip -> FSB5 bank (`sharedassets*.resource`) -> Vorbis packets decoded with `lewton` -> WAV bytes for
   Bevy's audio loader. FSB5 Vorbis streams leave out the identification and setup headers: the identification
   header is rebuilt from the FSB5 sample header; the setup header (looked up by the CRC32 in the stream) comes from
   the FMOD table in the player's own `UnityPlayer.dll` (32 headers: `{u64 ptr, u32 size, u32 crc32}` entries),
   or is regenerated with upstream libvorbis 1.3.4 (vendored, BSD-3), which writes the identical header for the
   (channels, rate, quality) FMOD's encoder used; the quality is found by searching -0.1..1.0 in 0.001 steps and
   cached per game build (`vorbis-<build>.tsv`). All 346 AudioClips decode; checked bit-exact against the old
   exports (Prophet_Loop_00, Prophet00, Kanye01, APMine_00, MortiniHit, FootstepsMetal02: max sample difference 0).
   The game runs with every exported sound removed. No third-party header tables are used.
5. Meshes and models: **in use for static prefabs.** `Mesh` objects are read in place (submeshes, vertex channels
   and streams incl. half floats, 16/32-bit indices, bind poses, streamed data; compressed meshes not yet).
   `owned_gltf.rs` builds the GLB of a prefab in memory in the layout of the old exports: one node per GameObject
   (root at the origin), Unity -> glTF mirrored on X (translation (-x,y,z), rotation (x,-y,-z,w)), one mesh per
   MeshFilter + MeshRenderer, submeshes sharing a material merged into one primitive, winding reversed, UV v
   flipped, identical vertices merged in first-use order, materials named after the Unity material with the main
   texture referenced by name (served by the texture fallback). Unity's built-in meshes are read from the loose
   `Resources/unity default resources` file. Weapons are found through the Resources table
   (`ResourceManager.m_Container`, "randomweapons/<name>"), other prefabs through a root-GameObject index.
   Checked against the 201 exported weapon / prop / projectile / hat / cigarette GLBs: 197 structurally identical
   (node names, order, TRS, mesh / primitive / vertex / index counts, positions, material names); the game runs with
   199 of them removed. Mesh files (body parts, first-person arms, hat / VFX meshes) are built the same way (root
   named after the mesh, `SubMesh_<i>` children, JOINTS_0 / WEIGHTS_0); same-named meshes are told apart in
   (file, path id) order, the exports' `_0` = the second. MeshFilters under SkinnedMeshRenderers count too.
   The port's own skinning takes bone hashes and bind poses from the Mesh in place (identical to the exports, test
   `owned_skin_matches_export`). All 233 exported weapon / prop / projectile / cosmetic / player / VFX GLBs:
   229 structurally identical; the game runs with all of them removed (player rig, dummy, ragdoll, arms checked).
   `models/map/ItemSpawner.glb` too. The map's 23 Bakery lightmaps (BC6H) are read in place, byte-identical to the
   old dumps (test `owned_lightmap_matches_export`; map screenshots unchanged). The map scene (`maps/TrainingRange_00.glb`) is built from its
   level file (found through BuildSettings.m_Scenes): scene roots reset like prefab roots, statically batched
   renderers take their submeshes of the combined world-space mesh back into local space (inverse world matrix,
   normals by its transpose). All 863 nodes and 425 meshes match the old export (transforms, bounds, materials;
   3 index counts differ); 25 MB instead of 580 MB, map screenshots unchanged. The reflection-probe cubemaps and the sky (Cubemap
   objects) are decoded in place into the same 6-face strips (faces in Unity's row order; checked against the old
   exports, mean difference 0.000 / 0.005 / 0.117). Collision meshes (map MeshColliders, popped-hat
   colliders) come from the Mesh in place too, identical to the old YAML decode (test `owned_mesh_matches_yaml`;
   the map still builds its 170 colliders). Still to do: compressed meshes.
5b. Fonts: **in use.** A font path not under `game/assets` is the embedded file (`m_FontData`) of the Unity Font of
   that name, byte-identical to the old exports (test `owned_font_matches_export`).
6. Prefabs, materials and scenes: **started.** `game/src/prefab_json.rs` builds the prefab data the game reads (the
   old extractor's JSON layout) from the installation: engine components with Unity 2021.3 player layouts
   (Transform / RectTransform, MeshFilter, Mesh / SkinnedMesh renderers, Box / Sphere / Capsule / Mesh colliders,
   CharacterController, Rigidbody, AudioSource with curves), scripts through the managed metadata, references as
   before ("ref:", "asset:<Dir>/<name>", built-ins, other prefabs, Resources paths, duplicate-name suffixes).
   `Prefab::load` falls back to it. Props: all 16 identical to the old exports (1,088 fields), in use (the map's 66
   props and the barrel work without the files). Animator, Light (incl. the player-only baking output) and
   ReflectionProbe added: 44 of 72 weapons build and match (16,415 fields; only a duplicate-material suffix and
   script-reference labels differ), the map's ItemSpawner matches. ParticleSystem, its renderer, LineRenderer, TrailRenderer and
   Cloth are read generically from `engine_types.txt` (Unity 2021.3 type trees generated from UnityPy's type tree
   package, MIT). **In use:** all weapons except DF_Cyst (legacy Animation component) and all 117 effect prefabs
   are built in place (`owned::data_text` / `data_exists` replace the file reads; the weapon list comes from the
   Resources table); weapons, effects, the Glock regression and the Prophet shot run without the files.
   Since then also in place: projectiles (loaded on first use), DF_Cyst, the player rig (408 objects), every hat
   (generic base classes such as `VariableReference<T>` resolve through the TypeSpec table with their type
   arguments), the training map scene (`scene_json`: all roots of the level, 863 objects; only root order and
   editor-only light probe groups differ), the scenes' light probes and the post-process profiles (both checked
   against the old exports by tests). Then the 96 effect materials (shader names from the SerializedShader type tree,
   all identical), the cosmetics list (`owned_cosmetics.rs`, from the MainMenu scene's CosmeticsManager), sprite
   rects (`m_RD.textureRect`), texture import settings (colour space / mips / wrap from the Texture2D header), the
   HUD Verlet settings and skin bind poses. `game/assets` now holds only the port's own shaders: no game data.
   Script layouts are fast (type index, cached field lists; the 3,585-object MainMenu scene builds in 0.6 s); name
   lists are cached per game build in the config folder (names and ids only).
   Still read from the old rip at runtime: animation clips and the Avatar, the MainMenu UI scene and TMP fonts,
   and the mixer gains.
7. Animation: AnimationClip curves (streamed / dense / constant) and humanoid muscle clips, Avatar.
8. Verify every system against the original game.
