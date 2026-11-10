# Validation

Restrafe is checked two ways: reference tests compare what is read from the installation with reference data, and
autotests play scripted sessions in the real game and log what happened.

## Reference tests

From `game/`:

```powershell
cargo test --release                              # format and logic tests; reference tests skip without data
$env:RESTRAFE_REF_CHECKS = "1"; cargo test --release   # every reference comparison
```

The reference tests need an installed STRAFTAT and reference data that is not part of this repository: data
exported from the same installation during development, before the in-place readers existed, and kept outside Git
(about 280 MB: the exported YAML of clips, controllers, the Avatar, materials, sprites, fonts and skinned meshes, the
normal-map and font-atlas images, and the old JSON data, light probes, lightmaps and collision meshes; the rest of
the old 47 GB export was deleted once nothing needed it). Without it they return early, so the public CI runs the
rest. What they compare, as of the in-place reading milestone:

| Test | Compares |
| --- | --- |
| `prefab_json_matches_export` | 2,836 prefab / scene objects, 129,288 fields (engine components and scripts) |
| `clips_match_export` | 149 animation clips, 5,038 curve components at every stored key |
| `controllers_match_export` | 71 animator controllers: layers, states, blend trees, transitions, conditions |
| `avatar_matches_export` | the humanoid Avatar: skeleton, axes, T-pose, muscles, masses |
| `material_docs_match_export` | 1,505 materials: shader, keywords, floats, colours, textures |
| `vfx_materials_match_export` | 96 effect materials |
| `tmp_fonts_match_export` | 23 TextMeshPro fonts (every glyph, every atlas byte) and 25 text materials |
| `normal_maps_match_export` | 68 normal maps unpacked like Unity's `UnpackNormal` |
| `texture_headers_match_meta` | colour space, mipmaps and wrap of 1,608 textures |
| `owned_skin_matches_export` | bind poses of 36 skinned meshes |
| `light_probes_match_export`, `pp_profiles_match_export`, `mixer_volumes_match_export`, `sprite_rects_match_export`, `cosmetics_match_export`, ... | light probes, post-processing profiles, mixer volumes, sprite rects, the cosmetics list |

The humanoid posing has its own ground truth inside the clips: Unity stores each clip's IK goals.
`RESTRAFE_GOAL_CHECK=<clip,...>` prints the RMS distance between the port's posed feet / hands and those goals.

## Autotests

`RESTRAFE_AUTOTEST=<name>` runs a scripted session (add `RESTRAFE_QUIET=1` to keep it muted and off-screen). Each
prints result lines and saves screenshots to `game/target/shots/`.

| Name | Checks |
| --- | --- |
| `glock` | fires at the dummy: health per shot, the kill on the 4th shot, the ragdoll |
| `hands` | picking up, swapping and dropping weapons in both hands |
| `weaponmenu` | spawning and firing any weapon (`RESTRAFE_WEAPON=<name>`) |
| `mapview`, `barrel`, `door`, `pigeon` | the training map: colliders, lights, props, item spawners, animated props |
| `walk`, `ramp`, `ledge`, `steep`, `walljump`, `wallstick`, `katanawj` | movement against the map's geometry |
| `hudturn` | the 3D HUD and its springs while turning |
| `menushot`, `cosmeticshot` | the settings, pause and customization menus |
| `suicide`, `dummyshot`, `shots`, `wallshot`, `disc`, `mapdisc`, `taunt` | death and kill cam, hit effects, projectiles |

Screenshots of two builds can be compared pixel by pixel; differences between two runs of the same build stay under
0.4% of the pixels.
