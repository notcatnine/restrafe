//! Builds the vendored libogg + libvorbis (BSD-3) used to regenerate FSB5 Vorbis setup headers (src/fsb5.rs).
fn main() {
    let (v, o) = ("vendor/libvorbis", "vendor/libogg");
    let mut b = cc::Build::new();
    b.include(format!("{v}/include")).include(format!("{v}/lib")).include(format!("{o}/include")).warnings(false).opt_level(2);
    for f in ["bitwise.c", "framing.c"] {
        b.file(format!("{o}/src/{f}"));
    }
    for f in [
        "analysis.c", "bitrate.c", "block.c", "codebook.c", "envelope.c", "floor0.c", "floor1.c", "info.c", "lookup.c", "lpc.c", "lsp.c", "mapping0.c", "mdct.c", "psy.c", "registry.c", "res0.c", "sharedbook.c", "smallft.c", "synthesis.c", "vorbisenc.c", "window.c",
    ] {
        b.file(format!("{v}/lib/{f}"));
    }
    b.file(format!("{v}/restrafe_shim.c"));
    b.compile("restrafe_vorbis");
    println!("cargo:rerun-if-changed=vendor/libvorbis");
    println!("cargo:rerun-if-changed=vendor/libogg");
}
