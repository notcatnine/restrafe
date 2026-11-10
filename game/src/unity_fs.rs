//! In-place reader for STRAFTAT's Unity data: the `UnityFS` bundle `STRAFTAT_Data/data.unity3d` (the game's
//! serialized files, read without extracting them) and the `SerializedFile` object tables inside it.
//!
//! Only the LZ4 blocks that cover a requested range are decompressed (`Bundle::read`), with a small block cache,
//! so opening the 6.4 GB bundle reads ~0.4 MB of directory data. Format: UnityFS version 6-8 (Unity 2021.3),
//! big-endian header and block table; SerializedFile version 22 (64-bit offsets).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

pub type Result<T> = std::result::Result<T, String>;

struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
    be: bool,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8], be: bool) -> Self {
        Cursor { b, p: 0, be }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| format!("bad length {n} at {}", self.p))?;
        let s = self.b.get(self.p..end).ok_or_else(|| format!("unexpected end of data at {}", self.p))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let a: [u8; 2] = self.take(2)?.try_into().unwrap();
        Ok(if self.be { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) })
    }
    fn u32(&mut self) -> Result<u32> {
        let a: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if self.be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) })
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64> {
        let a: [u8; 8] = self.take(8)?.try_into().unwrap();
        Ok(if self.be { u64::from_be_bytes(a) } else { u64::from_le_bytes(a) })
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }
    fn cstr(&mut self) -> Result<String> {
        let rest = &self.b[self.p..];
        let n = rest.iter().position(|&c| c == 0).ok_or("unterminated string")?;
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.p += n + 1;
        Ok(s)
    }
    fn align(&mut self, a: usize) {
        self.p = self.p.div_ceil(a) * a;
    }
}

fn decompress(kind: u16, src: &[u8], size: usize) -> Result<Vec<u8>> {
    match kind & 0x3f {
        0 => Ok(src.to_vec()),
        2 | 3 => lz4_flex::block::decompress(src, size).map_err(|e| format!("lz4: {e}")),
        k => Err(format!("unsupported bundle compression {k} (only none / LZ4 / LZ4HC)")),
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub path: String,
    pub offset: u64,
    pub size: u64,
    /// a loose serialized file next to the bundle (index into `Bundle::loose`) instead of bundle data
    pub loose: Option<usize>,
    #[allow(dead_code)]
    pub flags: u32,
}

struct Block {
    usize_: u64,
    csize: u64,
    flags: u16,
    /// offset of the compressed block in the file / of its data in the uncompressed stream
    file_off: u64,
    data_off: u64,
}

pub struct Bundle {
    file: Mutex<File>,
    pub unity_version: String,
    blocks: Vec<Block>,
    pub nodes: Vec<Node>,
    cache: Mutex<HashMap<usize, std::sync::Arc<Vec<u8>>>>,
    loose: Vec<Mutex<File>>,
}

impl Bundle {
    pub fn open(path: &Path) -> Result<Bundle> {
        let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut head = vec![0u8; 256];
        let n = f.read(&mut head).map_err(|e| e.to_string())?;
        head.truncate(n);
        let mut c = Cursor::new(&head, true);
        if c.cstr()? != "UnityFS" {
            return Err(format!("{} is not a UnityFS bundle", path.display()));
        }
        let version = c.u32()?;
        let _player = c.cstr()?;
        let unity_version = c.cstr()?;
        let _size = c.i64()?;
        let ci_size = c.u32()? as usize;
        let ui_size = c.u32()? as usize;
        let flags = c.u32()?;
        if version >= 7 {
            c.align(16);
        }
        let mut header_end = c.p as u64;
        let file_len = f.metadata().map_err(|e| e.to_string())?.len();
        let info_off = if flags & 0x80 != 0 { file_len - ci_size as u64 } else { header_end };
        let mut ci = vec![0u8; ci_size];
        f.seek(SeekFrom::Start(info_off)).and_then(|_| f.read_exact(&mut ci)).map_err(|e| e.to_string())?;
        if flags & 0x80 == 0 {
            header_end += ci_size as u64;
        }
        if flags & 0x200 != 0 {
            header_end = header_end.div_ceil(16) * 16;
        }
        let info = decompress(flags as u16, &ci, ui_size)?;
        let mut c = Cursor::new(&info, true);
        c.take(16)?; // uncompressed data hash
        let nb = c.i32()? as usize;
        let (mut blocks, mut fo, mut dof) = (Vec::with_capacity(nb), header_end, 0u64);
        for _ in 0..nb {
            let (us, cs, fl) = (c.u32()? as u64, c.u32()? as u64, c.u16()?);
            blocks.push(Block { usize_: us, csize: cs, flags: fl, file_off: fo, data_off: dof });
            fo += cs;
            dof += us;
        }
        let nn = c.i32()? as usize;
        let mut nodes = Vec::with_capacity(nn);
        for _ in 0..nn {
            let (offset, size, flags) = (c.u64()?, c.u64()?, c.u32()?);
            nodes.push(Node { path: c.cstr()?, offset, size, flags, loose: None });
        }
        Ok(Bundle { file: Mutex::new(f), unity_version, blocks, nodes, cache: Mutex::new(HashMap::new()), loose: Vec::new() })
    }

    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.path == name)
    }

    fn block(&self, i: usize) -> Result<std::sync::Arc<Vec<u8>>> {
        if let Some(b) = self.cache.lock().unwrap().get(&i) {
            return Ok(b.clone());
        }
        let b = &self.blocks[i];
        let mut raw = vec![0u8; b.csize as usize];
        {
            let mut f = self.file.lock().unwrap();
            f.seek(SeekFrom::Start(b.file_off)).and_then(|_| f.read_exact(&mut raw)).map_err(|e| e.to_string())?;
        }
        let data = std::sync::Arc::new(decompress(b.flags, &raw, b.usize_ as usize)?);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(i, data.clone());
        Ok(data)
    }

    /// `len` bytes at `off` of the uncompressed data stream, decompressing only the blocks that cover them.
    pub fn read(&self, off: u64, len: u64) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len as usize);
        let end = off + len;
        let mut i = self.blocks.partition_point(|b| b.data_off + b.usize_ <= off);
        while out.len() < len as usize {
            let b = self.blocks.get(i).ok_or("read past the end of the bundle")?;
            let data = self.block(i)?;
            let s = (off.max(b.data_off) - b.data_off) as usize;
            let e = (end.min(b.data_off + b.usize_) - b.data_off) as usize;
            out.extend_from_slice(&data[s..e]);
            i += 1;
        }
        Ok(out)
    }

    pub fn read_node(&self, n: &Node, off: u64, len: u64) -> Result<Vec<u8>> {
        let len = len.min(n.size.saturating_sub(off));
        if let Some(i) = n.loose {
            let mut f = self.loose[i].lock().unwrap();
            let mut buf = vec![0u8; len as usize];
            f.seek(SeekFrom::Start(off)).and_then(|_| f.read_exact(&mut buf)).map_err(|e| e.to_string())?;
            return Ok(buf);
        }
        self.read(n.offset + off, len)
    }

    /// Adds a loose serialized file (e.g. `Resources/unity default resources`) as a node named after its file name.
    pub fn add_loose(&mut self, path: &Path) -> Result<()> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let size = f.metadata().map_err(|e| e.to_string())?.len();
        self.loose.push(Mutex::new(f));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.nodes.push(Node { path: name, offset: 0, size, flags: 0, loose: Some(self.loose.len() - 1) });
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub path_id: i64,
    /// offset of the object's data inside the serialized file
    pub start: u64,
    pub size: u32,
    pub class_id: i32,
    pub script_type: i16,
}

#[derive(Clone, Debug)]
pub struct SerializedType {
    pub class_id: i32,
    pub script_id: Option<[u8; 16]>,
    pub has_type_tree: bool,
}

/// Object table of one serialized file (a bundle node such as `sharedassets0.assets` or `level0`).
pub struct SerializedFile {
    pub name: String,
    pub version: u32,
    pub unity_version: String,
    pub big_endian: bool,
    pub data_offset: u64,
    pub type_trees: bool,
    pub types: Vec<SerializedType>,
    pub objects: Vec<ObjectInfo>,
    pub externals: Vec<String>,
}

impl SerializedFile {
    pub fn open(bundle: &Bundle, node: &Node) -> Result<SerializedFile> {
        let head = bundle.read_node(node, 0, 48)?;
        let mut c = Cursor::new(&head, true);
        let (_meta_size, _fs, version, _data_offset) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
        if !(22..=30).contains(&version) {
            return Err(format!("{}: not a serialized file (or version {version}, needs 22+)", node.path));
        }
        let big_endian = c.u8()? != 0;
        c.take(3)?;
        let meta_size = c.u32()? as u64;
        let _file_size = c.i64()?;
        let data_offset = c.i64()? as u64;
        c.i64()?;
        let header_len = c.p as u64;
        // other bundle files (.resS / .resource data) fail these checks instead of being read as garbage
        if meta_size == 0 || header_len + meta_size > node.size || data_offset > node.size {
            return Err(format!("{}: not a serialized file", node.path));
        }
        let meta = bundle.read_node(node, header_len, meta_size)?;
        // a count can never need more bytes than the metadata holds
        let fits = |n: i32, each: usize| n >= 0 && (n as usize).saturating_mul(each) <= meta.len();
        let mut c = Cursor::new(&meta, big_endian);
        let unity_version = c.cstr()?;
        let _platform = c.i32()?;
        let type_trees = c.u8()? != 0;
        let nt = c.i32()?;
        if !fits(nt, 23) {
            return Err(format!("{}: not a serialized file", node.path));
        }
        let mut types = Vec::new();
        for _ in 0..nt {
            let class_id = c.i32()?;
            let _stripped = c.u8()?;
            let _script_index = c.u16()?;
            let script_id = if class_id == 114 { Some(c.take(16)?.try_into().unwrap()) } else { None };
            c.take(16)?; // old type hash
            if type_trees {
                // TypeTree blob: node count, string buffer size, 32-byte nodes, strings; then dependencies.
                let nodes = c.i32()? as usize;
                let strings = c.i32()? as usize;
                c.take(nodes.saturating_mul(32).saturating_add(strings))?;
                let deps = c.i32()? as usize;
                c.take(deps.saturating_mul(4))?;
            }
            types.push(SerializedType { class_id, script_id, has_type_tree: type_trees });
        }
        let no = c.i32()?;
        if !fits(no, 24) {
            return Err(format!("{}: not a serialized file", node.path));
        }
        let mut objects = Vec::with_capacity(no as usize);
        for _ in 0..no {
            c.align(4);
            let path_id = c.i64()?;
            let start = c.i64()? as u64;
            let size = c.u32()?;
            let ti = c.i32()?;
            let t = types.get(ti as usize).ok_or("bad type index")?;
            objects.push(ObjectInfo { path_id, start, size, class_id: t.class_id, script_type: ti as i16 });
        }
        // script types (path ids of MonoScripts), then externals
        let ns = c.i32()?;
        for _ in 0..ns {
            c.i32()?;
            c.align(4);
            c.i64()?;
        }
        let ne = c.i32()?;
        let mut externals = Vec::new();
        for _ in 0..ne {
            c.cstr()?;
            c.take(16)?;
            c.i32()?;
            externals.push(c.cstr()?);
        }
        Ok(SerializedFile { name: node.path.clone(), version, unity_version, big_endian, data_offset, type_trees, types, objects, externals })
    }

    /// Raw serialized bytes of one object.
    pub fn object_data(&self, bundle: &Bundle, node: &Node, o: &ObjectInfo) -> Result<Vec<u8>> {
        bundle.read_node(node, self.data_offset + o.start, o.size as u64)
    }
}

/// Little-endian reader over one object's serialized data (Unity 2021.3 player layout, no type trees).
pub struct ObjReader<'a> {
    c: Cursor<'a>,
}

impl<'a> ObjReader<'a> {
    pub fn new(b: &'a [u8], big_endian: bool) -> Self {
        ObjReader { c: Cursor::new(b, big_endian) }
    }
    pub fn i32(&mut self) -> Result<i32> {
        self.c.i32()
    }
    pub fn u32(&mut self) -> Result<u32> {
        self.c.u32()
    }
    pub fn u64(&mut self) -> Result<u64> {
        self.c.u64()
    }
    pub fn i64(&mut self) -> Result<i64> {
        self.c.i64()
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.c.u32()?))
    }
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.c.u8()? != 0)
    }
    pub fn align(&mut self) {
        self.c.align(4);
    }
    /// Aligned string (i32 length, UTF-8 bytes, align 4).
    pub fn string(&mut self) -> Result<String> {
        let n = self.c.i32()? as usize;
        let s = String::from_utf8_lossy(self.c.take(n)?).into_owned();
        self.c.align(4);
        Ok(s)
    }
    /// PPtr: file index (0 = this file, else externals[i - 1]) and path id.
    pub fn pptr(&mut self) -> Result<(i32, i64)> {
        Ok((self.c.i32()?, self.c.i64()?))
    }
    pub fn pos(&self) -> usize {
        self.c.p
    }
    pub fn byte(&mut self) -> Result<u8> {
        self.c.u8()
    }
    pub fn short(&mut self) -> Result<u16> {
        self.c.u16()
    }
}

/// Where an object's streamed payload lives (`m_Resource` / `m_StreamData`).
#[derive(Clone, Debug)]
pub struct StreamedResource {
    pub source: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct AudioClip {
    pub name: String,
    pub load_type: i32,
    pub channels: i32,
    pub frequency: i32,
    pub bits_per_sample: i32,
    pub length: f32,
    pub resource: StreamedResource,
    /// 0 PCM, 1 Vorbis, 2 ADPCM, 3 MP3, ...
    pub compression: i32,
}

impl AudioClip {
    pub fn read(data: &[u8], big_endian: bool) -> Result<AudioClip> {
        let mut r = ObjReader::new(data, big_endian);
        let name = r.string()?;
        let (load_type, channels, frequency, bits_per_sample) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let length = r.f32()?;
        r.bool()?; // m_IsTrackerFormat
        r.bool()?; // m_Ambisonic
        r.align();
        r.i32()?; // m_SubsoundIndex
        r.bool()?; // m_PreloadAudioData
        r.bool()?; // m_LoadInBackground
        r.bool()?; // m_Legacy3D
        r.align();
        let resource = StreamedResource { source: r.string()?, offset: r.u64()?, size: r.u64()? };
        let compression = r.i32()?;
        Ok(AudioClip { name, load_type, channels, frequency, bits_per_sample, length, resource, compression })
    }
}

/// The first object name of any named object (`m_Name` is the first field of every NamedObject).
pub fn object_name(data: &[u8], big_endian: bool) -> Option<String> {
    ObjReader::new(data, big_endian).string().ok().filter(|s| s.len() < 512)
}

/// Payload bytes of a streamed resource: a node of the bundle (`archive:/CAB-x/CAB-x.resS`) or a loose
/// `STRAFTAT_Data/<file>` next to it (`sharedassets0.resource`).
pub fn read_resource(bundle: &Bundle, data_dir: &Path, res: &StreamedResource) -> Result<Vec<u8>> {
    let file = res.source.rsplit('/').next().unwrap_or(&res.source);
    if let Some(n) = bundle.nodes.iter().find(|n| n.path == file || n.path.ends_with(&format!("/{file}"))) {
        return bundle.read_node(n, res.offset, res.size);
    }
    let mut f = File::open(data_dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
    let mut buf = vec![0u8; res.size as usize];
    f.seek(SeekFrom::Start(res.offset)).and_then(|_| f.read_exact(&mut buf)).map_err(|e| format!("{file}: {e}"))?;
    Ok(buf)
}

#[derive(Clone, Debug)]
pub struct Texture2D {
    pub name: String,
    pub width: i32,
    pub height: i32,
    /// TextureFormat (10 DXT1, 12 DXT5, 25 BC7, 4 RGBA32, ...)
    pub format: i32,
    pub mip_count: i32,
    pub color_space: i32,
    pub wrap: (i32, i32),
    pub filter: i32,
    pub aniso: i32,
    /// Pixel data stored inline (empty when streamed).
    pub image: Vec<u8>,
    pub stream: StreamedResource,
}

impl Texture2D {
    pub fn read(data: &[u8], big_endian: bool) -> Result<Texture2D> {
        let mut r = ObjReader::new(data, big_endian);
        let name = r.string()?;
        r.i32()?; // m_ForcedFallbackFormat
        r.bool()?; // m_DownscaleFallback
        r.bool()?; // m_IsAlphaChannelOptional
        r.align();
        let (width, height) = (r.i32()?, r.i32()?);
        r.u32()?; // m_CompleteImageSize
        r.i32()?; // m_MipsStripped
        let format = r.i32()?;
        let mip_count = r.i32()?;
        for _ in 0..4 {
            r.bool()?; // m_IsReadable, m_IsPreProcessed, m_IgnoreMasterTextureLimit, m_StreamingMipmaps
        }
        r.align();
        r.i32()?; // m_StreamingMipmapsPriority
        r.i32()?; // m_ImageCount
        r.i32()?; // m_TextureDimension
        let filter = r.i32()?;
        let aniso = r.i32()?;
        r.f32()?; // m_MipBias
        let wrap = (r.i32()?, r.i32()?);
        r.i32()?; // m_WrapW
        r.i32()?; // m_LightmapFormat
        let color_space = r.i32()?;
        let blob = r.i32()? as usize; // m_PlatformBlob
        for _ in 0..blob {
            r.c.u8()?;
        }
        r.align();
        let n = r.i32()? as usize; // image data
        let image = r.c.take(n)?.to_vec();
        r.align();
        let offset = r.u64()?;
        let size = r.u32()? as u64;
        let source = r.string()?;
        if width <= 0 || height <= 0 || width > 16384 || height > 16384 {
            return Err(format!("{name}: implausible size {width}x{height}"));
        }
        Ok(Texture2D { name, width, height, format, mip_count, color_space, wrap, filter, aniso, image, stream: StreamedResource { source, offset, size } })
    }
}

#[derive(Clone, Debug, Default)]
pub struct TexEnv {
    /// PPtr (file index, path id)
    pub texture: (i32, i64),
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct Material {
    pub name: String,
    pub shader: (i32, i64),
    pub keywords: Vec<String>,
    pub render_queue: i32,
    pub tags: Vec<(String, String)>,
    pub textures: Vec<(String, TexEnv)>,
    pub ints: Vec<(String, i32)>,
    pub floats: Vec<(String, f32)>,
    pub colors: Vec<(String, [f32; 4])>,
}

impl Material {
    pub fn read(data: &[u8], big_endian: bool) -> Result<Material> {
        let mut r = ObjReader::new(data, big_endian);
        let mut m = Material { name: r.string()?, shader: r.pptr()?, ..Default::default() };
        let strings = |r: &mut ObjReader| -> Result<Vec<String>> { (0..r.i32()?).map(|_| r.string()).collect() };
        m.keywords = strings(&mut r)?; // m_ValidKeywords
        strings(&mut r)?; // m_InvalidKeywords
        r.u32()?; // m_LightmapFlags
        r.bool()?; // m_EnableInstancingVariants
        r.bool()?; // m_DoubleSidedGI
        r.align();
        m.render_queue = r.i32()?;
        for _ in 0..r.i32()? {
            m.tags.push((r.string()?, r.string()?));
        }
        strings(&mut r)?; // disabledShaderPasses
        for _ in 0..r.i32()? {
            let k = r.string()?;
            let texture = r.pptr()?;
            let scale = [r.f32()?, r.f32()?];
            let offset = [r.f32()?, r.f32()?];
            m.textures.push((k, TexEnv { texture, scale, offset }));
        }
        for _ in 0..r.i32()? {
            m.ints.push((r.string()?, r.i32()?));
        }
        for _ in 0..r.i32()? {
            m.floats.push((r.string()?, r.f32()?));
        }
        for _ in 0..r.i32()? {
            m.colors.push((r.string()?, [r.f32()?, r.f32()?, r.f32()?, r.f32()?]));
        }
        Ok(m)
    }
}

#[derive(Clone, Debug)]
pub struct GameObject {
    pub components: Vec<(i32, i64)>,
    pub layer: u32,
    pub name: String,
    pub tag: u16,
    pub active: bool,
}

impl GameObject {
    pub fn read(data: &[u8], big_endian: bool) -> Result<GameObject> {
        let mut r = ObjReader::new(data, big_endian);
        let components = (0..r.i32()?).map(|_| r.pptr()).collect::<Result<Vec<_>>>()?;
        let layer = r.u32()?;
        let name = r.string()?;
        let tag = r.c.u16()?;
        let active = r.bool()?;
        Ok(GameObject { components, layer, name, tag, active })
    }
}

#[derive(Clone, Debug)]
pub struct Transform {
    pub game_object: (i32, i64),
    pub rotation: [f32; 4],
    pub position: [f32; 3],
    pub scale: [f32; 3],
    pub children: Vec<(i32, i64)>,
    pub father: (i32, i64),
}

impl Transform {
    /// Transform (class 4); RectTransform (224) starts with the same fields.
    pub fn read(data: &[u8], big_endian: bool) -> Result<Transform> {
        let mut r = ObjReader::new(data, big_endian);
        let game_object = r.pptr()?;
        let rotation = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        let position = [r.f32()?, r.f32()?, r.f32()?];
        let scale = [r.f32()?, r.f32()?, r.f32()?];
        let children = (0..r.i32()?).map(|_| r.pptr()).collect::<Result<Vec<_>>>()?;
        let father = r.pptr()?;
        Ok(Transform { game_object, rotation, position, scale, children, father })
    }
}

#[derive(Clone, Debug)]
pub struct Renderer {
    pub game_object: (i32, i64),
    pub enabled: bool,
    pub cast_shadows: u8,
    pub receive_shadows: bool,
    pub light_probe_usage: u8,
    pub reflection_probe_usage: u8,
    pub lightmap_index: u16,
    pub lightmap_st: [f32; 4],
    pub materials: Vec<(i32, i64)>,
    /// m_StaticBatchInfo: (first submesh, submesh count) in the combined mesh of a static batch (count 0 = none)
    pub static_batch: (u16, u16),
}

impl Renderer {
    /// Common head of MeshRenderer (23), SkinnedMeshRenderer (137), ParticleSystemRenderer (199) ...
    pub fn read(data: &[u8], big_endian: bool) -> Result<Renderer> {
        let mut r = ObjReader::new(data, big_endian);
        let game_object = r.pptr()?;
        let b: Vec<u8> = (0..10).map(|_| r.c.u8()).collect::<Result<_>>()?; // m_Enabled .. m_RayTraceProcedural
        r.align();
        r.u32()?; // m_RenderingLayerMask
        r.i32()?; // m_RendererPriority
        let lightmap_index = r.c.u16()?;
        r.c.u16()?; // m_LightmapIndexDynamic
        let lightmap_st = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        for _ in 0..4 {
            r.f32()?; // m_LightmapTilingOffsetDynamic
        }
        let materials = (0..r.i32()?).map(|_| r.pptr()).collect::<Result<Vec<_>>>()?;
        let static_batch = (r.c.u16().unwrap_or(0), r.c.u16().unwrap_or(0));
        Ok(Renderer { game_object, enabled: b[0] != 0, cast_shadows: b[1], receive_shadows: b[2] != 0, light_probe_usage: b[6], reflection_probe_usage: b[7], lightmap_index, lightmap_st, materials, static_batch })
    }
}

/// MeshFilter (33): the GameObject and its Mesh.
pub fn mesh_filter(data: &[u8], big_endian: bool) -> Result<((i32, i64), (i32, i64))> {
    let mut r = ObjReader::new(data, big_endian);
    Ok((r.pptr()?, r.pptr()?))
}

/// GPU block / pixel format of a Unity TextureFormat, for uploading the data as stored (no conversion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFormat {
    Bc1,
    Bc3,
    Bc4,
    Bc5,
    Bc6h,
    Bc7,
    Rgba8,
    Rgb8,
    R8,
    Alpha8,
    Rgba16f,
}

impl GpuFormat {
    pub fn from_unity(f: i32) -> Option<GpuFormat> {
        Some(match f {
            10 => GpuFormat::Bc1,
            12 => GpuFormat::Bc3,
            26 => GpuFormat::Bc4,
            27 => GpuFormat::Bc5,
            24 => GpuFormat::Bc6h,
            25 => GpuFormat::Bc7,
            4 => GpuFormat::Rgba8,
            3 => GpuFormat::Rgb8,
            63 => GpuFormat::R8,
            1 => GpuFormat::Alpha8,
            17 => GpuFormat::Rgba16f,
            _ => return None,
        })
    }
    /// Bytes of one mip level.
    pub fn level_size(self, w: u32, h: u32) -> usize {
        let blocks = |b: usize| (w.div_ceil(4) as usize) * (h.div_ceil(4) as usize) * b;
        match self {
            GpuFormat::Bc1 | GpuFormat::Bc4 => blocks(8),
            GpuFormat::Bc3 | GpuFormat::Bc5 | GpuFormat::Bc6h | GpuFormat::Bc7 => blocks(16),
            GpuFormat::Rgba8 => (w * h * 4) as usize,
            GpuFormat::Rgb8 => (w * h * 3) as usize,
            GpuFormat::R8 | GpuFormat::Alpha8 => (w * h) as usize,
            GpuFormat::Rgba16f => (w * h * 8) as usize,
        }
    }
}

impl Texture2D {
    /// The texture's pixel data as stored (all mips, largest first), inline or from its `.resS` stream.
    pub fn pixels(&self, bundle: &Bundle, data_dir: &Path) -> Result<Vec<u8>> {
        if !self.image.is_empty() {
            return Ok(self.image.clone());
        }
        read_resource(bundle, data_dir, &self.stream)
    }
    /// Expected byte size of the full mip chain, when the format is known.
    pub fn expected_size(&self) -> Option<usize> {
        let f = GpuFormat::from_unity(self.format)?;
        Some((0..self.mip_count.max(1) as u32).map(|m| f.level_size((self.width as u32 >> m).max(1), (self.height as u32 >> m).max(1))).sum())
    }
}

#[derive(Clone, Debug)]
pub struct SubMesh {
    pub first_byte: u32,
    pub index_count: u32,
    pub topology: i32,
    pub base_vertex: u32,
    pub first_vertex: u32,
    pub vertex_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct ChannelInfo {
    pub stream: u8,
    pub offset: u8,
    /// VertexFormat: 0 Float32, 1 Float16, 2 UNorm8, 3 SNorm8, 4 UNorm16, 5 SNorm16, 6 UInt8, 7 SInt8, 8 UInt16,
    /// 9 SInt16, 10 UInt32, 11 SInt32
    pub format: u8,
    /// components (low 4 bits)
    pub dimension: u8,
}

/// Unity Mesh (class 43), 2021.3 player layout. Vertex attributes are decoded to f32 per channel
/// (0 position, 1 normal, 2 tangent, 3 color, 4..11 uv0..uv7, 12 blend weights, 13 blend indices).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub name: String,
    pub submeshes: Vec<SubMesh>,
    pub bind_poses: Vec<[f32; 16]>,
    pub bone_name_hashes: Vec<u32>,
    pub root_bone_name_hash: u32,
    pub index_format: i32,
    pub indices: Vec<u32>,
    pub vertex_count: u32,
    pub channels: Vec<ChannelInfo>,
    /// decoded channels: index = channel number, each `vertex_count * dimension` values
    pub attributes: Vec<Option<(u8, Vec<f32>)>>,
    pub compressed: bool,
}

fn format_size(f: u8) -> usize {
    match f {
        0 | 10 | 11 => 4,
        1 | 4 | 5 | 8 | 9 => 2,
        _ => 1,
    }
}

impl Mesh {
    pub fn read(data: &[u8], big_endian: bool, bundle: &Bundle, data_dir: &Path) -> Result<Mesh> {
        let mut r = ObjReader::new(data, big_endian);
        let mut m = Mesh { name: r.string()?, ..Default::default() };
        for _ in 0..r.i32()? {
            let s = SubMesh { first_byte: r.u32()?, index_count: r.u32()?, topology: r.i32()?, base_vertex: r.u32()?, first_vertex: r.u32()?, vertex_count: r.u32()? };
            for _ in 0..6 {
                r.f32()?; // localAABB
            }
            m.submeshes.push(s);
        }
        // m_Shapes: vertices (40 bytes each), shapes (12 bytes each), channels, fullWeights
        let nv = r.i32()? as usize;
        r.c.take(nv * 40)?;
        let ns = r.i32()? as usize;
        r.c.take(ns * 12)?;
        for _ in 0..r.i32()? {
            r.string()?;
            r.u32()?;
            r.i32()?;
            r.i32()?;
        }
        let nw = r.i32()? as usize;
        r.c.take(nw * 4)?;
        for _ in 0..r.i32()? {
            let mut b = [0f32; 16];
            for v in &mut b {
                *v = r.f32()?;
            }
            m.bind_poses.push(b);
        }
        m.bone_name_hashes = (0..r.i32()?).map(|_| r.u32()).collect::<Result<_>>()?;
        m.root_bone_name_hash = r.u32()?;
        let na = r.i32()? as usize; // m_BonesAABB
        r.c.take(na * 24)?;
        let nvb = r.i32()? as usize; // m_VariableBoneCountWeights
        r.c.take(nvb * 4)?;
        let compression = r.c.u8()?;
        r.bool()?; // m_IsReadable
        r.bool()?; // m_KeepVertices
        r.bool()?; // m_KeepIndices
        r.align();
        m.index_format = r.i32()?;
        let ni = r.i32()? as usize;
        let ib = r.c.take(ni)?.to_vec();
        r.align();
        m.indices = if m.index_format == 1 {
            ib.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect()
        } else {
            ib.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32).collect()
        };
        m.vertex_count = r.u32()?;
        for _ in 0..r.i32()? {
            m.channels.push(ChannelInfo { stream: r.c.u8()?, offset: r.c.u8()?, format: r.c.u8()?, dimension: r.c.u8()? });
        }
        let nd = r.i32()? as usize;
        let mut vdata = r.c.take(nd)?.to_vec();
        r.align();
        // m_CompressedMesh: 10 packed bit vectors + m_UVInfo
        let pbv = |r: &mut ObjReader, float: bool| -> Result<usize> {
            let n = r.u32()? as usize;
            if float {
                r.f32()?;
                r.f32()?;
            }
            let len = r.i32()? as usize;
            r.c.take(len)?;
            r.align();
            r.c.u8()?;
            r.align();
            Ok(n)
        };
        let cv = pbv(&mut r, true)?;
        for float in [true, true, true, false, false, false, true, false, false] {
            pbv(&mut r, float)?;
        }
        r.u32()?; // m_UVInfo
        m.compressed = compression != 0 || cv > 0;
        for _ in 0..6 {
            r.f32()?; // m_LocalAABB
        }
        r.i32()?; // m_MeshUsageFlags
        let n1 = r.i32()? as usize;
        r.c.take(n1)?;
        r.align();
        let n2 = r.i32()? as usize;
        r.c.take(n2)?;
        r.align();
        r.f32()?; // m_MeshMetrics[0]
        r.f32()?; // m_MeshMetrics[1]
        let offset = r.u64()?;
        let size = r.u32()? as u64;
        let source = r.string()?;
        if vdata.is_empty() && size > 0 {
            vdata = read_resource(bundle, data_dir, &StreamedResource { source, offset, size })?;
        }
        if m.compressed {
            return Err(format!("{}: compressed meshes are not supported yet", m.name));
        }
        // streams: each holds the channels with that stream index, interleaved; streams start 16-aligned
        let nstreams = m.channels.iter().map(|c| c.stream as usize + 1).max().unwrap_or(0);
        let mut stream_off = 0usize;
        let mut attrs: Vec<Option<(u8, Vec<f32>)>> = vec![None; m.channels.len()];
        for s in 0..nstreams {
            let stride: usize = m.channels.iter().filter(|c| c.stream as usize == s && c.dimension & 0xf > 0).map(|c| format_size(c.format) * (c.dimension & 0xf) as usize).sum();
            for (ci, c) in m.channels.iter().enumerate() {
                let dim = (c.dimension & 0xf) as usize;
                if c.stream as usize != s || dim == 0 {
                    continue;
                }
                let fs = format_size(c.format);
                let mut out = Vec::with_capacity(m.vertex_count as usize * dim);
                for v in 0..m.vertex_count as usize {
                    for k in 0..dim {
                        let o = stream_off + v * stride + c.offset as usize + k * fs;
                        let b = vdata.get(o..o + fs).ok_or_else(|| format!("{}: vertex data out of range", m.name))?;
                        let x = match c.format {
                            0 => f32::from_le_bytes(b.try_into().unwrap()),
                            1 => half_to_f32(u16::from_le_bytes([b[0], b[1]])),
                            2 => b[0] as f32 / 255.0,
                            3 => (b[0] as i8 as f32 / 127.0).max(-1.0),
                            4 => u16::from_le_bytes([b[0], b[1]]) as f32 / 65535.0,
                            5 => (i16::from_le_bytes([b[0], b[1]]) as f32 / 32767.0).max(-1.0),
                            6 => b[0] as f32,
                            7 => b[0] as i8 as f32,
                            8 => u16::from_le_bytes([b[0], b[1]]) as f32,
                            9 => i16::from_le_bytes([b[0], b[1]]) as f32,
                            10 => u32::from_le_bytes(b.try_into().unwrap()) as f32,
                            _ => i32::from_le_bytes(b.try_into().unwrap()) as f32,
                        };
                        out.push(x);
                    }
                }
                attrs[ci] = Some((dim as u8, out));
            }
            stream_off += stride * m.vertex_count as usize;
            stream_off = stream_off.div_ceil(16) * 16;
        }
        m.attributes = attrs;
        Ok(m)
    }

    pub fn attr(&self, channel: usize) -> Option<(u8, &[f32])> {
        self.attributes.get(channel)?.as_ref().map(|(d, v)| (*d, v.as_slice()))
    }
}

fn half_to_f32(h: u16) -> f32 {
    let s = ((h >> 15) & 1) as u32;
    let e = ((h >> 10) & 0x1f) as i32;
    let f = (h & 0x3ff) as u32;
    let bits = if e == 0 {
        if f == 0 {
            s << 31
        } else {
            let mut e = -1i32;
            let mut f = f;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            (s << 31) | (((e + 127 - 14) as u32) << 23) | ((f & 0x3ff) << 13)
        }
    } else if e == 31 {
        (s << 31) | 0x7f80_0000 | (f << 13)
    } else {
        (s << 31) | (((e - 15 + 127) as u32) << 23) | (f << 13)
    };
    f32::from_bits(bits)
}

/// The font file embedded in a Unity Font (class 128): `m_FontData` (TrueType / OpenType bytes as imported).
pub fn font_data(data: &[u8], big_endian: bool) -> Result<Vec<u8>> {
    let mut r = ObjReader::new(data, big_endian);
    r.string()?; // m_Name
    r.f32()?; // m_LineSpacing
    r.pptr()?; // m_DefaultMaterial
    r.f32()?; // m_FontSize
    r.pptr()?; // m_Texture
    r.i32()?; // m_AsciiStartOffset
    r.f32()?; // m_Tracking
    r.i32()?; // m_CharacterSpacing
    r.i32()?; // m_CharacterPadding
    r.i32()?; // m_ConvertCase
    let n = r.i32()? as usize; // m_CharacterRects: index, uv, vert, advance, flipped (+ align) = 44 bytes
    r.c.take(n * 44)?;
    let k = r.i32()? as usize; // m_KerningValues: ((u16, u16), f32)
    r.c.take(k * 8)?;
    r.f32()?; // m_PixelScale
    let len = r.i32()? as usize;
    let bytes = r.c.take(len)?.to_vec();
    match bytes.get(..4) {
        Some([0, 1, 0, 0]) | Some(b"OTTO") | Some(b"true") | Some(b"ttcf") => Ok(bytes),
        _ => Err("font data is not a TrueType / OpenType file".into()),
    }
}

/// A serialized script field value read with the layout from the managed metadata (first stage: primitives,
/// strings, object references, Unity math structs and arrays / Lists of those).
#[derive(Clone, Debug)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// PPtr (file index, path id)
    Ref(i32, i64),
    Floats(Vec<f32>),
    List(Vec<Value>),
}

fn struct_floats(name: &str) -> Option<usize> {
    Some(match name {
        "Vector2" => 2,
        "Vector3" => 3,
        "Vector4" | "Quaternion" | "Color" | "Rect" => 4,
        "Bounds" => 6,
        "Matrix4x4" => 16,
        _ => return None,
    })
}

/// Reads one field value; `Err` for types this stage does not handle yet.
pub fn read_field(r: &mut ObjReader, ty: &crate::dotnet_meta::TypeSig, asm: &crate::dotnet_meta::Assemblies) -> Result<Value> {
    use crate::dotnet_meta::TypeSig as T;
    Ok(match ty {
        T::Prim(p) => {
            let v = match *p {
                "bool" => Value::Bool(r.c.u8()? != 0),
                "byte" => Value::Int(r.c.u8()? as i64),
                "sbyte" => Value::Int(r.c.u8()? as i8 as i64),
                "char" | "ushort" => Value::Int(r.c.u16()? as i64),
                "short" => Value::Int(r.c.u16()? as i16 as i64),
                "int" => Value::Int(r.i32()? as i64),
                "uint" => Value::Int(r.u32()? as i64),
                "long" => Value::Int(r.i64()?),
                "ulong" => Value::Int(r.u64()? as i64),
                "float" => Value::Float(r.f32()? as f64),
                "double" => Value::Float(f64::from_bits(r.u64()?)),
                _ => return Err(format!("primitive {p}")),
            };
            r.align(); // script fields smaller than 4 bytes are padded
            v
        }
        T::String => Value::Str(r.string()?),
        T::Named { value: true, name, .. } if struct_floats(name).is_some() => Value::Floats((0..struct_floats(name).unwrap()).map(|_| r.f32()).collect::<Result<_>>()?),
        T::Named { value: true, name, .. } if name == "LayerMask" || name == "Color32" => Value::Int(r.u32()? as i64),
        T::Named { namespace, name, .. } if asm.is_enum(namespace, name) => {
            let v = Value::Int(r.i32()? as i64);
            v
        }
        T::Named { namespace, name, .. } if asm.is_unity_object(namespace, name) => {
            // UnityEngine.Object-derived reference (GameObject, Sprite, Material, script components ...)
            let (f, p) = r.pptr()?;
            Value::Ref(f, p)
        }
        T::Named { namespace, name, .. } if asm.find(namespace, name).is_some_and(|t| t.flags & 0x2000 != 0) => {
            // [Serializable] class / struct: its fields inline
            let t = asm.find(namespace, name).unwrap();
            let mut v = Vec::new();
            for f in asm.serialized_fields(t) {
                v.push(read_field(r, &f.ty, asm).map_err(|e| format!("{name}.{}: {e}", f.name))?);
            }
            Value::List(v)
        }
        T::Array(e) => {
            let n = r.i32()? as usize;
            let v = (0..n).map(|_| read_field(r, e, asm)).collect::<Result<_>>()?;
            r.align();
            Value::List(v)
        }
        T::Generic { base, args } if matches!(&**base, T::Named { name, .. } if name == "List`1") && args.len() == 1 => {
            let n = r.i32()? as usize;
            let v = (0..n).map(|_| read_field(r, &args[0], asm)).collect::<Result<_>>()?;
            r.align();
            Value::List(v)
        }
        t => return Err(format!("field type {t} not handled yet")),
    })
}

/// MonoBehaviour (class 114): base fields, then the script's serialized fields in declaration order.
pub fn read_monobehaviour(data: &[u8], big_endian: bool, fields: &[crate::dotnet_meta::FieldDef], asm: &crate::dotnet_meta::Assemblies) -> Result<Vec<(String, Value)>> {
    let mut r = ObjReader::new(data, big_endian);
    r.pptr()?; // m_GameObject
    r.c.u8()?; // m_Enabled
    r.align();
    r.pptr()?; // m_Script
    r.string()?; // m_Name
    let mut out = Vec::new();
    for f in fields {
        let v = read_field(&mut r, &f.ty, asm).map_err(|e| format!("{}: {e} (after {} fields)", f.name, out.len()))?;
        out.push((f.name.clone(), v));
    }
    Ok(out)
}
