# libvorbis 1.3.4 (vendored)

Upstream Xiph.Org libvorbis 1.3.4 (`lib/` and `include/`), BSD-3-Clause (see `COPYING`); the build helpers
`psytune.c`, `barkmel.c`, `tone.c`, `misc.c` and the autotools files are left out. Restrafe uses it only to
regenerate the Vorbis setup headers that FMOD's FSB5 streams leave out (`game/src/fsb5.rs`): libvorbis builds the
same header from (channels, sample rate, quality) that FMOD's encoder wrote, which is matched by the CRC32 stored in
each stream. Built by `game/build.rs` together with `../libogg` (libogg, BSD-3-Clause).
