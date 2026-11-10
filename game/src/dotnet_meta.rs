//! Minimal ECMA-335 metadata reader for the game's own managed assemblies (`STRAFTAT_Data/Managed/*.dll`, read in
//! place): type definitions with their fields (declaration order, flags, signature types). The player build has no
//! type trees, so the MonoBehaviour layouts Unity serialized are derived from these declarations.

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub flags: u16,
    pub ty: TypeSig,
    /// custom attribute type names (e.g. "SerializeField", "NonSerializedAttribute")
    pub attributes: Vec<String>,
}

impl FieldDef {
    pub fn is_static(&self) -> bool {
        self.flags & 0x10 != 0
    }
    pub fn is_public(&self) -> bool {
        self.flags & 0x7 == 0x6
    }
    pub fn is_not_serialized(&self) -> bool {
        self.flags & 0x80 != 0
    }
    pub fn is_literal_or_initonly(&self) -> bool {
        self.flags & 0x60 != 0
    }
    pub fn has_attribute(&self, name: &str) -> bool {
        self.attributes.iter().any(|a| a == name || a.strip_suffix("Attribute") == Some(name))
    }
    /// Unity's rule for fields of a serialized class: public or [SerializeField], not static / const / readonly /
    /// [NonSerialized].
    pub fn unity_serialized(&self) -> bool {
        !self.is_static() && !self.is_literal_or_initonly() && !self.is_not_serialized() && (self.is_public() || self.has_attribute("SerializeField"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeSig {
    Prim(&'static str),
    String,
    Object,
    /// class / valuetype: (is value type, TypeDefOrRef coded index resolved to (namespace, name))
    Named { value: bool, namespace: String, name: String },
    Array(Box<TypeSig>),
    Generic { base: Box<TypeSig>, args: Vec<TypeSig> },
    GenericParam(u32),
    Other(u8),
}

#[derive(Clone, Debug)]
pub struct TypeDef {
    pub namespace: String,
    pub name: String,
    pub flags: u32,
    /// base type (namespace, name), when it is a TypeDef / TypeRef
    pub extends: Option<(String, String)>,
    /// the base type is a generic instance (TypeSpec), e.g. UnityEvent<bool>
    pub extends_generic: bool,
    /// that generic instance's signature
    pub extends_spec: Option<TypeSig>,
    pub fields: Vec<FieldDef>,
}

pub struct Assembly {
    pub types: Vec<TypeDef>,
}

struct Rd<'a> {
    b: &'a [u8],
}
impl Rd<'_> {
    fn u16(&self, o: usize) -> Result<u16> {
        self.b.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated".into())
    }
    fn u32(&self, o: usize) -> Result<u32> {
        self.b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated".into())
    }
}

impl Assembly {
    pub fn parse(dll: &[u8]) -> Result<Assembly> {
        let r = Rd { b: dll };
        let pe = r.u32(0x3c)? as usize;
        if dll.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err("not a PE file".into());
        }
        let nsec = r.u16(pe + 6)? as usize;
        let optsize = r.u16(pe + 20)? as usize;
        let opt = pe + 24;
        let pe32plus = r.u16(opt)? == 0x20b;
        let dd = opt + if pe32plus { 112 } else { 96 };
        let cli_rva = r.u32(dd + 14 * 8)? as usize;
        let secs: Vec<(usize, usize, usize)> = (0..nsec)
            .map(|i| {
                let o = opt + optsize + 40 * i;
                Ok((r.u32(o + 12)? as usize, r.u32(o + 8)?.max(r.u32(o + 16)?) as usize, r.u32(o + 20)? as usize))
            })
            .collect::<Result<_>>()?;
        let off = |rva: usize| secs.iter().find(|(va, sz, _)| *va <= rva && rva < va + sz).map(|(va, _, raw)| rva - va + raw).ok_or_else(|| format!("rva {rva:x} outside sections"));
        let cli = off(cli_rva)?;
        let meta = off(r.u32(cli + 8)? as usize)?;
        if r.u32(meta)? != 0x424a_5342 {
            return Err("no metadata root".into());
        }
        let vlen = r.u32(meta + 12)? as usize;
        let mut p = meta + 16 + vlen;
        let nstreams = r.u16(p + 2)? as usize;
        p += 4;
        let mut streams = std::collections::HashMap::new();
        for _ in 0..nstreams {
            let (so, ss) = (r.u32(p)? as usize, r.u32(p + 4)? as usize);
            p += 8;
            let end = dll[p..].iter().position(|&c| c == 0).ok_or("stream name")?;
            let name = String::from_utf8_lossy(&dll[p..p + end]).into_owned();
            p += (end + 4) & !3;
            streams.insert(name, (meta + so, ss));
        }
        let (tables, _) = *streams.get("#~").or(streams.get("#-")).ok_or("no #~ stream")?;
        let (strings, _) = *streams.get("#Strings").ok_or("no #Strings")?;
        let (blob, _) = *streams.get("#Blob").ok_or("no #Blob")?;
        let heap = dll[tables + 6];
        let (str_w, guid_w, blob_w) = (if heap & 1 != 0 { 4 } else { 2 }, if heap & 2 != 0 { 4 } else { 2 }, if heap & 4 != 0 { 4 } else { 2 });
        let valid = u64::from_le_bytes(dll[tables + 8..tables + 16].try_into().unwrap());
        let mut rows = [0usize; 64];
        let mut q = tables + 24;
        for (t, row) in rows.iter_mut().enumerate() {
            if valid & (1 << t) != 0 {
                *row = r.u32(q)? as usize;
                q += 4;
            }
        }
        let idx_w = |t: usize| if rows[t] < 1 << 16 { 2 } else { 4 };
        let coded_w = |tabs: &[usize], bits: u32| if tabs.iter().map(|&t| rows[t]).max().unwrap_or(0) < 1 << (16 - bits) { 2 } else { 4 };
        let resolution_scope = coded_w(&[0x00, 0x1a, 0x23, 0x01], 2);
        let typedef_or_ref = coded_w(&[0x02, 0x01, 0x1b], 2);
        let field_list = if rows[0x03] > 0 { idx_w(0x03) } else { idx_w(0x04) };
        let method_list = if rows[0x05] > 0 { idx_w(0x05) } else { idx_w(0x06) };
        let param_list = if rows[0x07] > 0 { idx_w(0x07) } else { idx_w(0x08) };
        let member_ref_parent = coded_w(&[0x02, 0x01, 0x1a, 0x06, 0x1b], 3);
        let has_constant = coded_w(&[0x04, 0x08, 0x17], 2);
        let has_custom_attribute = coded_w(&[0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0a, 0x00, 0x0e, 0x17, 0x14, 0x11, 0x1a, 0x1b, 0x20, 0x23, 0x26, 0x27, 0x28, 0x2a, 0x2c, 0x2b], 5);
        let custom_attribute_type = coded_w(&[0x06, 0x0a], 3);
        let row_size = [
            2 + str_w + 3 * guid_w,                                    // 0x00 Module
            resolution_scope + 2 * str_w,                              // 0x01 TypeRef
            4 + 2 * str_w + typedef_or_ref + field_list + method_list, // 0x02 TypeDef
            idx_w(0x04),                                               // 0x03 FieldPtr
            2 + str_w + blob_w,                                        // 0x04 Field
            idx_w(0x06),                                               // 0x05 MethodPtr
            4 + 2 + 2 + str_w + blob_w + param_list,                   // 0x06 MethodDef
            idx_w(0x08),                                               // 0x07 ParamPtr
            2 + 2 + str_w,                                             // 0x08 Param
            idx_w(0x02) + typedef_or_ref,                              // 0x09 InterfaceImpl
            member_ref_parent + str_w + blob_w,                        // 0x0a MemberRef
            2 + has_constant + blob_w,                                 // 0x0b Constant
            has_custom_attribute + custom_attribute_type + blob_w,     // 0x0c CustomAttribute
            coded_w(&[0x04, 0x08], 1) + blob_w,                        // 0x0d FieldMarshal
            2 + coded_w(&[0x02, 0x06, 0x20], 2) + blob_w,              // 0x0e DeclSecurity
            2 + 4 + idx_w(0x02),                                       // 0x0f ClassLayout
            4 + idx_w(0x04),                                           // 0x10 FieldLayout
            blob_w,                                                    // 0x11 StandAloneSig
            idx_w(0x02) + idx_w(0x14),                                 // 0x12 EventMap
            idx_w(0x14),                                               // 0x13 EventPtr
            2 + str_w + typedef_or_ref,                                // 0x14 Event
            idx_w(0x02) + idx_w(0x17),                                 // 0x15 PropertyMap
            idx_w(0x17),                                               // 0x16 PropertyPtr
            2 + str_w + blob_w,                                        // 0x17 Property
            2 + idx_w(0x06) + coded_w(&[0x14, 0x17], 1),               // 0x18 MethodSemantics
            idx_w(0x02) + 2 * coded_w(&[0x06, 0x0a], 1),               // 0x19 MethodImpl
            str_w,                                                     // 0x1a ModuleRef
            blob_w,                                                    // 0x1b TypeSpec
        ];
        let start = |t: usize| -> usize { q + (0..t).map(|k| rows[k] * row_size[k]).sum::<usize>() };
        let rd = |o: usize, w: usize| -> Result<usize> { Ok(if w == 2 { r.u16(o)? as usize } else { r.u32(o)? as usize }) };
        let string = |i: usize| -> String {
            let s = &dll[strings + i..];
            let n = s.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf8_lossy(&s[..n]).into_owned()
        };
        // TypeRef names
        let tr0 = start(0x01);
        let typeref = |i: usize| -> Result<(String, String)> {
            let o = tr0 + (i - 1) * row_size[1];
            Ok((string(rd(o + resolution_scope + str_w, str_w)?), string(rd(o + resolution_scope, str_w)?)))
        };
        let td0 = start(0x02);
        let typedef_name = |i: usize| -> Result<(String, String)> {
            let o = td0 + (i - 1) * row_size[2];
            Ok((string(rd(o + 4 + str_w, str_w)?), string(rd(o + 4, str_w)?)))
        };
        let tdor = |c: usize| -> Result<Option<(String, String)>> {
            let (tag, i) = (c & 3, c >> 2);
            if i == 0 {
                return Ok(None);
            }
            Ok(match tag {
                0 => Some(typedef_name(i)?),
                1 => Some(typeref(i)?),
                _ => None,
            })
        };
        // compressed unsigned integer of a signature blob
        fn uncompress(b: &[u8], p: &mut usize) -> usize {
            let x = b[*p] as usize;
            if x & 0x80 == 0 {
                *p += 1;
                x
            } else if x & 0xc0 == 0x80 {
                *p += 2;
                ((x & 0x3f) << 8) | b[*p - 1] as usize
            } else {
                *p += 4;
                ((x & 0x1f) << 24) | ((b[*p - 3] as usize) << 16) | ((b[*p - 2] as usize) << 8) | b[*p - 1] as usize
            }
        }
        fn sig_type(b: &[u8], p: &mut usize, tdor: &dyn Fn(usize) -> Result<Option<(String, String)>>) -> Result<TypeSig> {
            let e = *b.get(*p).ok_or("sig")?;
            *p += 1;
            Ok(match e {
                0x02 => TypeSig::Prim("bool"),
                0x03 => TypeSig::Prim("char"),
                0x04 => TypeSig::Prim("sbyte"),
                0x05 => TypeSig::Prim("byte"),
                0x06 => TypeSig::Prim("short"),
                0x07 => TypeSig::Prim("ushort"),
                0x08 => TypeSig::Prim("int"),
                0x09 => TypeSig::Prim("uint"),
                0x0a => TypeSig::Prim("long"),
                0x0b => TypeSig::Prim("ulong"),
                0x0c => TypeSig::Prim("float"),
                0x0d => TypeSig::Prim("double"),
                0x0e => TypeSig::String,
                0x1c => TypeSig::Object,
                0x11 | 0x12 => {
                    let c = uncompress(b, p);
                    let (ns, n) = tdor(c)?.unwrap_or_default();
                    TypeSig::Named { value: e == 0x11, namespace: ns, name: n }
                }
                0x1d => TypeSig::Array(Box::new(sig_type(b, p, tdor)?)),
                0x15 => {
                    let base = sig_type(b, p, tdor)?;
                    let n = uncompress(b, p);
                    let args = (0..n).map(|_| sig_type(b, p, tdor)).collect::<Result<_>>()?;
                    TypeSig::Generic { base: Box::new(base), args }
                }
                0x13 | 0x1e => TypeSig::GenericParam(uncompress(b, p) as u32),
                x => TypeSig::Other(x),
            })
        }
        // CustomAttribute rows whose parent is a Field (tag 1): attribute type = MemberRef(.ctor).Class (a TypeRef)
        let mut field_attrs: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
        let (ca0, mr0) = (start(0x0c), start(0x0a));
        // first method of every TypeDef (an attribute defined in the same assembly has a MethodDef constructor)
        let method_starts: Vec<usize> = (0..rows[0x02]).map(|k| rd(td0 + k * row_size[2] + 4 + 2 * str_w + typedef_or_ref + field_list, method_list).unwrap_or(0)).collect();
        for k in 0..rows[0x0c] {
            let o = ca0 + k * row_size[0x0c];
            let parent = rd(o, has_custom_attribute)?;
            if parent & 31 != 1 {
                continue;
            }
            let ty = rd(o + has_custom_attribute, custom_attribute_type)?;
            let name = match ty & 7 {
                3 => {
                    let mr = mr0 + ((ty >> 3) - 1) * row_size[0x0a];
                    let class = rd(mr, member_ref_parent)?;
                    match class & 7 {
                        1 => typeref(class >> 3)?.1,
                        0 => typedef_name(class >> 3)?.1,
                        _ => continue,
                    }
                }
                2 => {
                    // MethodDef: the TypeDef whose method range holds it
                    let m = ty >> 3;
                    let Some(k) = method_starts.iter().rposition(|&st| st != 0 && st <= m) else { continue };
                    typedef_name(k + 1)?.1
                }
                _ => continue,
            };
            field_attrs.entry(parent >> 5).or_default().push(name);
        }
        let f0 = start(0x04);
        let field = |i: usize| -> Result<FieldDef> {
            let o = f0 + (i - 1) * row_size[4];
            let flags = r.u16(o)?;
            let name = string(rd(o + 2, str_w)?);
            let bi = rd(o + 2 + str_w, blob_w)?;
            let mut p = blob + bi;
            let _len = uncompress(dll, &mut p);
            p += 1; // FIELD (0x06)
            let ty = sig_type(dll, &mut p, &tdor).unwrap_or(TypeSig::Other(0));
            Ok(FieldDef { name, flags, ty, attributes: field_attrs.get(&i).cloned().unwrap_or_default() })
        };
        let mut types = Vec::with_capacity(rows[0x02]);
        let nfields = rows[0x04];
        for i in 1..=rows[0x02] {
            let o = td0 + (i - 1) * row_size[2];
            let flags = r.u32(o)?;
            let name = string(rd(o + 4, str_w)?);
            let namespace = string(rd(o + 4 + str_w, str_w)?);
            let ext_coded = rd(o + 4 + 2 * str_w, typedef_or_ref)?;
            let extends = tdor(ext_coded)?;
            let extends_generic = ext_coded & 3 == 2 && ext_coded >> 2 != 0;
            // the generic instance a TypeSpec base names, e.g. VariableReference<VerletHierarchySettings>
            let extends_spec = if extends_generic && ext_coded >> 2 <= rows[0x1b] {
                let o = start(0x1b) + ((ext_coded >> 2) - 1) * row_size[0x1b];
                let mut p = blob + rd(o, blob_w)?;
                let _len = uncompress(dll, &mut p);
                sig_type(dll, &mut p, &tdor).ok()
            } else {
                None
            };
            let first = rd(o + 4 + 2 * str_w + typedef_or_ref, field_list)?;
            let next = if i < rows[0x02] { rd(o + row_size[2] + 4 + 2 * str_w + typedef_or_ref, field_list)? } else { nfields + 1 };
            let fields = (first..next.min(nfields + 1)).map(&field).collect::<Result<_>>()?;
            types.push(TypeDef { namespace, name, flags, extends, extends_generic, extends_spec, fields });
        }
        Ok(Assembly { types })
    }

    pub fn find(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name == name)
    }
}

impl std::fmt::Display for TypeSig {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TypeSig::Prim(p) => write!(f, "{p}"),
            TypeSig::String => write!(f, "string"),
            TypeSig::Object => write!(f, "object"),
            TypeSig::Named { name, .. } => write!(f, "{name}"),
            TypeSig::Array(e) => write!(f, "{e}[]"),
            TypeSig::Generic { base, args } => write!(f, "{base}<{}>", args.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")),
            TypeSig::GenericParam(i) => write!(f, "T{i}"),
            TypeSig::Other(x) => write!(f, "?{x:02x}"),
        }
    }
}

/// Every assembly of `Managed/` (types resolved across them by namespace + name).
pub struct Assemblies {
    pub list: Vec<(String, Assembly)>,
    /// (namespace, name) -> (assembly, type) of the first definition
    index: std::collections::HashMap<(String, String), (usize, usize)>,
    /// serialized fields per (assembly, type) without generic arguments
    fields_cache: std::sync::Mutex<std::collections::HashMap<(usize, usize), Vec<FieldDef>>>,
}

impl Assemblies {
    pub fn load(managed: &std::path::Path) -> Result<Assemblies> {
        let mut list = Vec::new();
        for e in std::fs::read_dir(managed).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("dll") {
                continue;
            }
            let Ok(b) = std::fs::read(&p) else { continue };
            if let Ok(a) = Assembly::parse(&b) {
                list.push((p.file_stem().unwrap().to_string_lossy().into_owned(), a));
            }
        }
        let mut index = std::collections::HashMap::new();
        for (ai, (_, a)) in list.iter().enumerate() {
            for (ti, t) in a.types.iter().enumerate() {
                index.entry((t.namespace.clone(), t.name.clone())).or_insert((ai, ti));
            }
        }
        Ok(Assemblies { list, index, fields_cache: Default::default() })
    }

    pub fn find(&self, namespace: &str, name: &str) -> Option<&TypeDef> {
        let &(a, t) = self.index.get(&(namespace.to_string(), name.to_string()))?;
        Some(&self.list[a].1.types[t])
    }

    /// (assembly, type) position of a TypeDef of these assemblies.
    fn position(&self, t: &TypeDef) -> Option<(usize, usize)> {
        let &(a, i) = self.index.get(&(t.namespace.clone(), t.name.clone()))?;
        std::ptr::eq(&self.list[a].1.types[i], t).then_some((a, i))
    }

    pub fn find_by_name(&self, name: &str) -> Option<&TypeDef> {
        self.list.iter().find(|(n, _)| n == "Assembly-CSharp").and_then(|(_, a)| a.find(name)).or_else(|| self.list.iter().find_map(|(_, a)| a.find(name)))
    }

    fn is_engine_root(ns: &str, name: &str) -> bool {
        (ns == "UnityEngine" && matches!(name, "MonoBehaviour" | "Behaviour" | "Component" | "Object" | "ScriptableObject")) || (ns == "System" && name == "Object")
    }

    /// Serialized fields of `t` with its base classes' first (Unity's order).
    pub fn serialized_fields(&self, t: &TypeDef) -> Vec<FieldDef> {
        let Some(pos) = self.position(t) else { return self.serialized_fields_with(t, &[]) };
        if let Some(v) = self.fields_cache.lock().unwrap().get(&pos) {
            return v.clone();
        }
        let v = self.serialized_fields_with(t, &[]);
        self.fields_cache.lock().unwrap().insert(pos, v.clone());
        v
    }

    /// `args`: the generic arguments `t` is instantiated with (its fields of type `!i` become `args[i]`).
    fn serialized_fields_with(&self, t: &TypeDef, args: &[TypeSig]) -> Vec<FieldDef> {
        fn subst(ty: &TypeSig, args: &[TypeSig]) -> TypeSig {
            match ty {
                TypeSig::GenericParam(i) => args.get(*i as usize).cloned().unwrap_or(ty.clone()),
                TypeSig::Array(e) => TypeSig::Array(Box::new(subst(e, args))),
                TypeSig::Generic { base, args: a } => TypeSig::Generic { base: base.clone(), args: a.iter().map(|x| subst(x, args)).collect() },
                _ => ty.clone(),
            }
        }
        let mut out = Vec::new();
        if let Some(TypeSig::Generic { base, args: bargs }) = &t.extends_spec {
            if let TypeSig::Named { namespace, name, .. } = &**base {
                if !Self::is_engine_root(namespace, name) {
                    if let Some(b) = self.find(namespace, name) {
                        let bargs: Vec<TypeSig> = bargs.iter().map(|a| subst(a, args)).collect();
                        out = self.serialized_fields_with(b, &bargs);
                    }
                }
            }
        }
        // Generic bases are not resolved (TypeSpec); the one that matters for Unity data is UnityEvent<T>, whose
        // serialized state lives in UnityEventBase (m_PersistentCalls).
        if out.is_empty() && t.extends.is_none() && t.extends_generic && t.name.contains("Event") {
            if let Some(b) = self.find("UnityEngine.Events", "UnityEventBase") {
                out = self.serialized_fields(b);
            }
        }
        if let Some((ns, name)) = &t.extends {
            if !Self::is_engine_root(ns, name) && !(ns == "System" && (name == "ValueType" || name == "Enum")) {
                if let Some(b) = self.find(ns, name) {
                    out = self.serialized_fields_with(b, &[]);
                }
            }
        }
        // a field of the class's own type (e.g. VerletParticle.parent) would recurse: Unity leaves it out
        // (a UnityEngine.Object of its own type is just a reference)
        let own_is_ref = self.is_unity_object(&t.namespace, &t.name);
        let own = |ty: &TypeSig| !own_is_ref && match ty {
            TypeSig::Named { name, namespace, .. } => *name == t.name && *namespace == t.namespace,
            TypeSig::Array(e) => matches!(&**e, TypeSig::Named { name, .. } if *name == t.name),
            TypeSig::Generic { args, .. } => args.iter().any(|a| matches!(a, TypeSig::Named { name, .. } if *name == t.name)),
            _ => false,
        };
        out.extend(
            t.fields
                .iter()
                .map(|f| if args.is_empty() { f.clone() } else { FieldDef { ty: subst(&f.ty, args), ..f.clone() } })
                .filter(|f| f.unity_serialized() && self.is_serializable(&f.ty, 0) && !own(&f.ty)),
        );
        out
    }

    /// Unity serializes a field only when its type is serializable: primitives, strings, enums, UnityEngine.Object
    /// references, Unity's math / colour types, [Serializable] classes and structs, and arrays / Lists of those.
    pub fn is_serializable(&self, ty: &TypeSig, depth: u32) -> bool {
        match ty {
            TypeSig::Prim(_) | TypeSig::String => true,
            TypeSig::Named { namespace, name, .. } => {
                matches!(name.as_str(), "Vector2" | "Vector3" | "Vector4" | "Quaternion" | "Color" | "Color32" | "Rect" | "Bounds" | "Matrix4x4" | "LayerMask" | "AnimationCurve" | "Gradient" | "RectOffset" | "Vector2Int" | "Vector3Int")
                    || self.is_enum(namespace, name)
                    || self.is_unity_object(namespace, name)
                    || self.find(namespace, name).is_some_and(|t| t.flags & 0x2000 != 0)
            }
            TypeSig::Array(e) => depth == 0 && !matches!(**e, TypeSig::Array(_) | TypeSig::Generic { .. }) && self.is_serializable(e, depth + 1),
            TypeSig::Generic { base, args } => {
                depth == 0 && matches!(&**base, TypeSig::Named { name, .. } if name == "List`1") && args.len() == 1 && !matches!(args[0], TypeSig::Array(_) | TypeSig::Generic { .. }) && self.is_serializable(&args[0], depth + 1)
            }
            _ => false,
        }
    }

    /// Whether a type derives from UnityEngine.Object (serialized as a PPtr).
    pub fn is_unity_object(&self, ns: &str, name: &str) -> bool {
        if ns == "UnityEngine" && name == "Object" {
            return true;
        }
        if ns.starts_with("UnityEngine") && self.find(ns, name).is_none() {
            return true;
        }
        let mut cur = self.find(ns, name);
        for _ in 0..32 {
            let Some(t) = cur else { return false };
            let base = t.extends.clone().or_else(|| match &t.extends_spec {
                Some(TypeSig::Generic { base, .. }) => match &**base {
                    TypeSig::Named { namespace, name, .. } => Some((namespace.clone(), name.clone())),
                    _ => None,
                },
                _ => None,
            });
            match base {
                Some((bns, bn)) if bns == "UnityEngine" && matches!(bn.as_str(), "Object" | "MonoBehaviour" | "Behaviour" | "Component" | "ScriptableObject") => return true,
                Some((bns, bn)) => cur = self.find(&bns, &bn),
                None => return false,
            }
        }
        false
    }

    /// Underlying primitive of an enum (`value__` field type), e.g. "int", "byte".
    pub fn enum_underlying(&self, ns: &str, name: &str) -> &'static str {
        let t = self.find(ns, name);
        match t.and_then(|t| t.fields.iter().find(|f| f.name == "value__")).map(|f| &f.ty) {
            Some(TypeSig::Prim(p)) => p,
            _ => "int",
        }
    }

    pub fn is_enum(&self, ns: &str, name: &str) -> bool {
        self.find(ns, name).and_then(|t| t.extends.as_ref()).is_some_and(|(bns, bn)| bns == "System" && bn == "Enum")
    }
}
