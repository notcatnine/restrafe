//! Lists what Restrafe reads in place from the player's STRAFTAT installation: the install found by the same
//! discovery as the game (`--game` > RESTRAFE_ROOT > remembered folder > Steam), the bundle's serialized files and
//! the object count per Unity class.
//!
//! `cargo run --release --bin inspect -- [--game <folder>] [--file <node name>]`

#[path = "../install.rs"]
mod install;
#[path = "../dotnet_meta.rs"]
#[allow(dead_code)] // shared with the game; inspect uses part of it
mod dotnet_meta;
#[path = "../fsb5.rs"]
mod fsb5;
#[path = "../unity_fs.rs"]
#[allow(dead_code)]
mod unity_fs;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let root = install::discover(arg("--game").map(Into::into)).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let build = install::installed_build(&root);
    println!("STRAFTAT: {} (Steam build {}, tested {})", root.display(), build.as_deref().unwrap_or("?"), install::TESTED_BUILD);
    let t = std::time::Instant::now();
    let bundle = unity_fs::Bundle::open(&root.join(install::DATA_DIR).join(install::BUNDLE)).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    println!("bundle: Unity {}, {} nodes, opened in {:?}", bundle.unity_version, bundle.nodes.len(), t.elapsed());
    let only = arg("--file");
    if args.iter().any(|a| a == "--nodes") {
        for n in bundle.nodes.iter().filter(|n| !n.path.starts_with("sharedassets") && !n.path.starts_with("level") && !n.path.starts_with("CAB-")) {
            println!("node {} ({} bytes)", n.path, n.size);
        }
    }
    let (mut files, mut objects) = (0, 0);
    for n in &bundle.nodes {
        if only.as_ref().is_some_and(|o| *o != n.path) || n.path.ends_with(".resS") || n.path.ends_with(".resource") {
            continue;
        }
        match unity_fs::SerializedFile::open(&bundle, n) {
            Ok(sf) => {
                files += 1;
                objects += sf.objects.len();
                if only.is_some() {
                    let mut by: std::collections::BTreeMap<i32, usize> = Default::default();
                    for o in &sf.objects {
                        *by.entry(o.class_id).or_default() += 1;
                    }
                    println!("{}: v{} typetrees={} {} objects, externals {:?}", sf.name, sf.version, sf.type_trees, sf.objects.len(), sf.externals);
                    for (c, k) in by {
                        println!("  class {c}: {k}");
                    }
                }
            }
            Err(e) => println!("{}: {e}", n.path),
        }
    }
    println!("{files} serialized files, {objects} objects, total {:?}", t.elapsed());
    // --audio <name>: find the AudioClip in place and check its FSB5 payload.
    if let Some(want) = arg("--audio") {
        let data_dir = root.join(install::DATA_DIR);
        let mut found = 0;
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 83) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).as_deref() != Some(want.as_str()) {
                    continue;
                }
                found += 1;
                match unity_fs::AudioClip::read(&d, sf.big_endian) {
                    Ok(c) => {
                        let payload = unity_fs::read_resource(&bundle, &data_dir, &c.resource);
                        let magic = payload.as_ref().map(|p| String::from_utf8_lossy(&p[..4.min(p.len())]).into_owned());
                        println!("{} in {} (path id {}): {} ch {} Hz {:.3} s, compression {}, {} @ {} + {} bytes -> {:?}", c.name, n.path, o.path_id, c.channels, c.frequency, c.length, c.compression, c.resource.source, c.resource.offset, c.resource.size, magic);
                    }
                    Err(e) => println!("{want}: {e}"),
                }
            }
        }
        println!("{found} AudioClip(s) named {want}");
    }
    // --texture <name>: find the Texture2D in place and check where its pixels are.
    if let Some(want) = arg("--texture") {
        let data_dir = root.join(install::DATA_DIR);
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 28) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).as_deref() != Some(want.as_str()) {
                    continue;
                }
                match unity_fs::Texture2D::read(&d, sf.big_endian) {
                    Ok(t) => {
                        let bytes = t.pixels(&bundle, &data_dir).map(|p| format!("{} bytes as {:?} (expected {:?})", p.len(), unity_fs::GpuFormat::from_unity(t.format), t.expected_size()));
                        println!("{} in {} (path id {}): {}x{} format {} mips {} colorspace {} wrap {:?} filter {} aniso {}, pixels {:?} ({} @ {})", t.name, n.path, o.path_id, t.width, t.height, t.format, t.mip_count, t.color_space, t.wrap, t.filter, t.aniso, bytes, t.stream.source, t.stream.offset);
                    }
                    Err(e) => println!("{want}: {e}"),
                }
            }
        }
    }
    // --material <name>
    if let Some(want) = arg("--material") {
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 21) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).as_deref() != Some(want.as_str()) {
                    continue;
                }
                match unity_fs::Material::read(&d, sf.big_endian) {
                    Ok(m) => println!("{} in {} (path id {}): shader {:?} queue {} keywords {:?}
  tex {:?}
  floats {:?}
  colors {:?}", m.name, n.path, o.path_id, m.shader, m.render_queue, m.keywords, m.textures.iter().filter(|t| t.1.texture.1 != 0).collect::<Vec<_>>(), m.floats.iter().filter(|f| ["_Mode", "_SrcBlend", "_DstBlend", "_Cull", "_Float0", "_SmissiveBoost"].contains(&f.0.as_str())).collect::<Vec<_>>(), m.colors.iter().filter(|c| c.0 == "_Color").collect::<Vec<_>>()),
                    Err(e) => println!("{want}: {e}"),
                }
            }
        }
    }
    // --prefab <root GameObject name>: hierarchy with active flags, layers, local scales and component classes.
    if let Some(want) = arg("--prefab") {
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            let by_id: std::collections::HashMap<i64, &unity_fs::ObjectInfo> = sf.objects.iter().map(|o| (o.path_id, o)).collect();
            let read = |id: i64| by_id.get(&id).and_then(|o| sf.object_data(&bundle, n, o).ok());
            for o in sf.objects.iter().filter(|o| o.class_id == 1) {
                let Some(go) = read(o.path_id).and_then(|d| unity_fs::GameObject::read(&d, sf.big_endian).ok()) else { continue };
                if go.name != want {
                    continue;
                }
                fn walk(go_id: i64, depth: usize, sf: &unity_fs::SerializedFile, by_id: &std::collections::HashMap<i64, &unity_fs::ObjectInfo>, read: &dyn Fn(i64) -> Option<Vec<u8>>) {
                    let Some(go) = read(go_id).and_then(|d| unity_fs::GameObject::read(&d, sf.big_endian).ok()) else { return };
                    let comps: Vec<i32> = go.components.iter().filter_map(|c| by_id.get(&c.1).map(|o| o.class_id)).collect();
                    let tf = go.components.iter().find(|c| by_id.get(&c.1).is_some_and(|o| o.class_id == 4 || o.class_id == 224)).and_then(|c| read(c.1)).and_then(|d| unity_fs::Transform::read(&d, sf.big_endian).ok());
                    println!("{}{}{} layer {} scale {:?} classes {:?}", "  ".repeat(depth), go.name, if go.active { "" } else { " (inactive)" }, go.layer, tf.as_ref().map(|t| t.scale), comps);
                    for c in &go.components {
                        let Some(o) = by_id.get(&c.1) else { continue };
                        let Some(d) = read(c.1) else { continue };
                        let name_of = |p: (i32, i64)| if p.0 == 0 { read(p.1).and_then(|d| unity_fs::object_name(&d, sf.big_endian)).unwrap_or_default() } else { format!("{}:{}", sf.externals.get(p.0 as usize - 1).map(String::as_str).unwrap_or("?"), p.1) };
                        if o.class_id == 23 || o.class_id == 199 || o.class_id == 137 {
                            if let Ok(rd) = unity_fs::Renderer::read(&d, sf.big_endian) {
                                println!("{}  renderer {} enabled {} materials {:?}", "  ".repeat(depth), o.class_id, rd.enabled, rd.materials.iter().map(|&m| name_of(m)).collect::<Vec<_>>());
                            }
                        } else if o.class_id == 33 {
                            if let Ok((_, mesh)) = unity_fs::mesh_filter(&d, sf.big_endian) {
                                println!("{}  mesh {}", "  ".repeat(depth), name_of(mesh));
                            }
                        }
                    }
                    for ch in tf.map(|t| t.children).unwrap_or_default() {
                        if let Some(t) = read(ch.1).and_then(|d| unity_fs::Transform::read(&d, sf.big_endian).ok()) {
                            walk(t.game_object.1, depth + 1, sf, by_id, read);
                        }
                    }
                }
                println!("== {} path id {}", n.path, o.path_id);
                walk(o.path_id, 0, &sf, &by_id, &read);
            }
        }
    }
    // --decode <texture>: mip 0 as raw RGBA (rows as stored) -> target/decoded_<name>.rgba (u32 w, u32 h, pixels)
    if let Some(want) = arg("--decode") {
        let data_dir = root.join(install::DATA_DIR);
        'outer: for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 28) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).as_deref() != Some(want.as_str()) {
                    continue;
                }
                let t = unity_fs::Texture2D::read(&d, sf.big_endian).unwrap();
                let px = t.pixels(&bundle, &data_dir).unwrap();
                let (w, h) = (t.width as usize, t.height as usize);
                let mut out = vec![0u32; w * h];
                let r = match t.format {
                    10 => texture2ddecoder::decode_bc1(&px, w, h, &mut out),
                    12 => texture2ddecoder::decode_bc3(&px, w, h, &mut out),
                    25 => texture2ddecoder::decode_bc7(&px, w, h, &mut out),
                    f => Err(if f == 4 { "rgba" } else { "format" }),
                };
                println!("decode {want}: {}x{} format {} -> {:?}", w, h, t.format, r);
                let mut bytes = Vec::with_capacity(8 + w * h * 4);
                bytes.extend_from_slice(&(w as u32).to_le_bytes());
                bytes.extend_from_slice(&(h as u32).to_le_bytes());
                for p in out {
                    let [b, g, r, a] = p.to_le_bytes();
                    bytes.extend_from_slice(&[r, g, b, a]);
                }
                std::fs::write(format!("target/decoded_{want}.rgba"), bytes).unwrap();
                break 'outer;
            }
        }
    }
    // --wav <clip> [--ogg <exported .ogg to compare>]: decode an AudioClip in place -> target/<clip>.wav
    // --audio-all: decode every AudioClip and report the ones that fail.
    let want_wav = arg("--wav");
    if want_wav.is_some() || args.iter().any(|a| a == "--audio-all") {
        let data_dir = root.join(install::DATA_DIR);
        let dll = std::fs::read(root.join("UnityPlayer.dll")).expect("UnityPlayer.dll");
        let setups = fsb5::SetupSource { dll: fsb5::fmod_vorbis_setups(&dll).unwrap(), qualities: Default::default() };
        println!("FMOD Vorbis setups in UnityPlayer.dll: {}", setups.dll.len());
        let lookup = |c: u32, ch: u16, r: u32| setups.get(c, ch, r);
        let (mut ok, mut bad) = (0, Vec::new());
        let t = std::time::Instant::now();
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 83) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                let Ok(c) = unity_fs::AudioClip::read(&d, sf.big_endian) else { continue };
                if want_wav.as_ref().is_some_and(|w| *w != c.name) {
                    continue;
                }
                let r = unity_fs::read_resource(&bundle, &data_dir, &c.resource).and_then(|b| fsb5::decode(&b, &lookup));
                match r {
                    Ok(pcm) => {
                        ok += 1;
                        if want_wav.is_some() {
                            std::fs::write(format!("target/{}.wav", c.name), pcm.wav()).unwrap();
                            println!("{}: {} ch {} Hz {} frames ({:.3} s, clip says {:.3} s)", c.name, pcm.channels, pcm.rate, pcm.samples.len() / pcm.channels as usize, pcm.samples.len() as f32 / pcm.channels as f32 / pcm.rate as f32, c.length);
                            if let Some(ogg) = arg("--ogg") {
                                let mut r = lewton::inside_ogg::OggStreamReader::new(std::fs::File::open(&ogg).unwrap()).unwrap();
                                let mut ref_s: Vec<i16> = Vec::new();
                                while let Some(p) = r.read_dec_packet_itl().unwrap() {
                                    ref_s.extend(p);
                                }
                                let n = ref_s.len().min(pcm.samples.len());
                                let diff: i64 = (0..n).map(|i| (ref_s[i] as i64 - pcm.samples[i] as i64).abs()).max().unwrap_or(0);
                                println!("  vs {ogg}: {} vs {} samples, max abs diff {diff}", ref_s.len(), pcm.samples.len());
                            }
                        }
                    }
                    Err(e) => bad.push(format!("{}: {e}", c.name)),
                }
            }
        }
        println!("decoded {ok} AudioClips, {} failed, {:?}", bad.len(), t.elapsed());
        for b in bad.iter() {
            println!("  {b}");
        }
    }
    // --mesh <name>: read a Mesh in place and print its layout and first vertices
    if let Some(want) = arg("--mesh") {
        let data_dir = root.join(install::DATA_DIR);
        for n in &bundle.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 43) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).as_deref() != Some(want.as_str()) {
                    continue;
                }
                match unity_fs::Mesh::read(&d, sf.big_endian, &bundle, &data_dir) {
                    Ok(m) => {
                        println!("{} in {} (path id {}): {} vertices, {} indices (format {}), submeshes {:?}", m.name, n.path, o.path_id, m.vertex_count, m.indices.len(), m.index_format, m.submeshes.iter().map(|s| (s.first_byte, s.index_count, s.topology, s.base_vertex, s.vertex_count)).collect::<Vec<_>>());
                        println!("  channels {:?}", m.channels.iter().enumerate().filter(|(_, c)| c.dimension & 0xf > 0).map(|(i, c)| (i, c.stream, c.offset, c.format, c.dimension)).collect::<Vec<_>>());
                        println!("  bones {} bindposes {} root hash {}", m.bone_name_hashes.len(), m.bind_poses.len(), m.root_bone_name_hash);
                        for ch in [0usize, 1, 4] {
                            if let Some((dim, v)) = m.attr(ch) {
                                println!("  ch{ch} first: {:?}", &v[..(dim as usize * 3).min(v.len())]);
                            }
                        }
                        println!("  first tris {:?}", &m.indices[..9.min(m.indices.len())]);
                    }
                    Err(e) => println!("{want}: {e}"),
                }
            }
        }
    }
    // --class <name>: fields of a class from STRAFTAT_Data/Managed/Assembly-CSharp.dll (read in place)
    if let Some(want) = arg("--class") {
        let dll = std::fs::read(root.join(install::DATA_DIR).join("Managed").join("Assembly-CSharp.dll")).expect("Assembly-CSharp.dll");
        let asm = dotnet_meta::Assembly::parse(&dll).unwrap();
        println!("Assembly-CSharp: {} types", asm.types.len());
        let all = dotnet_meta::Assemblies::load(&root.join(install::DATA_DIR).join("Managed")).unwrap();
        if let Some(t) = asm.find(&want).or_else(|| all.find_by_name(&want)) {
            println!("serialized: {:?}", all.serialized_fields(t).iter().map(|f| f.name.clone()).collect::<Vec<_>>());
            println!("class {}.{} : {:?} (flags {:#x})", t.namespace, t.name, t.extends, t.flags);
            for f in &t.fields {
                println!("  {} {:<6} {} {} {:?}", if f.unity_serialized() { "S" } else { " " }, if f.is_public() { "public" } else { "" }, f.ty, f.name, f.attributes);
            }
        }
    }
    // --mb <class> [--file <node>]: decode that script's MonoBehaviours in place (first 3) with the metadata layout
    if let Some(class) = arg("--mb") {
        let asms = dotnet_meta::Assemblies::load(&root.join(install::DATA_DIR).join("Managed")).unwrap();
        let t = asms.find_by_name(&class).expect("class");
        let fields = asms.serialized_fields(t);
        println!("{} serialized fields (with base classes): {:?}", class, fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
        let mut shown = 0;
        let mut script_files: std::collections::HashMap<String, Option<unity_fs::SerializedFile>> = std::collections::HashMap::new();
        'scan: for n in &bundle.nodes {
            if only.as_ref().is_some_and(|o| *o != n.path) {
                continue;
            }
            let Ok(sf) = unity_fs::SerializedFile::open(&bundle, n) else { continue };
            let by_id: std::collections::HashMap<i64, &unity_fs::ObjectInfo> = sf.objects.iter().map(|o| (o.path_id, o)).collect();
            for o in sf.objects.iter().filter(|o| o.class_id == 114) {
                let Ok(d) = sf.object_data(&bundle, n, o) else { continue };
                // m_Script -> MonoScript (class 115) in this file: m_Name, m_ExecutionOrder, m_PropertiesHash, m_ClassName
                let mut r = unity_fs::ObjReader::new(&d, sf.big_endian);
                let (_, _) = r.pptr().unwrap();
                let _ = r.bool();
                r.align();
                let (sfid, spid) = r.pptr().unwrap();
                // the MonoScript may live in another file (externals[sfid - 1])
                let sd = if sfid == 0 {
                    let Some(so) = by_id.get(&spid) else { continue };
                    let Ok(sd) = sf.object_data(&bundle, n, so) else { continue };
                    sd
                } else {
                    let Some(ext) = sf.externals.get(sfid as usize - 1) else { continue };
                    let name = ext.rsplit('/').next().unwrap_or(ext);
                    let Some(en) = bundle.nodes.iter().find(|x| x.path == name) else { continue };
                    let Some(esf) = script_files.entry(en.path.clone()).or_insert_with(|| unity_fs::SerializedFile::open(&bundle, en).ok()).as_ref() else { continue };
                    let Some(so) = esf.objects.iter().find(|o| o.path_id == spid) else { continue };
                    let Ok(sd) = esf.object_data(&bundle, en, so) else { continue };
                    sd
                };
                let mut sr = unity_fs::ObjReader::new(&sd, sf.big_endian);
                let _ = sr.string();
                let _ = sr.i32();
                for _ in 0..4 {
                    let _ = sr.u32();
                }
                if sr.string().ok().as_deref() != Some(class.as_str()) {
                    continue;
                }
                match unity_fs::read_monobehaviour(&d, sf.big_endian, &fields, &asms) {
                    Ok(v) => println!("{} path id {}: {:?}", n.path, o.path_id, v),
                    Err(e) => println!("{} path id {}: {e}", n.path, o.path_id),
                }
                shown += 1;
                if shown >= 3 {
                    break 'scan;
                }
            }
        }
    }
}
