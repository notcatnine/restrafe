//! FMOD FSB5 sound banks (the payload of Unity AudioClips) decoded in memory.
//!
//! FSB5 Vorbis streams leave out the Vorbis identification and setup headers: the identification header is rebuilt
//! from the FSB5 sample header, and the setup header (codebooks) is looked up by the CRC32 stored in the stream in the
//! table FMOD keeps inside the player's own `UnityPlayer.dll` ([`fmod_vorbis_setups`]). No header data ships with
//! Restrafe. Packets are decoded with `lewton`; PCM16 samples are returned (and can be wrapped as WAV).

use std::collections::HashMap;

pub type Result<T> = std::result::Result<T, String>;

/// Vorbis setup headers in the FMOD table of a Unity player DLL: CRC32 -> header bytes.
/// The table is an array of `{u64 pointer, u32 size, u32 crc32}` pointing at the headers (which start "\x05vorbis").
pub fn fmod_vorbis_setups(dll: &[u8]) -> Result<HashMap<u32, Vec<u8>>> {
    let u16_at = |o: usize| dll.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap())).ok_or("short PE");
    let u32_at = |o: usize| dll.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or("short PE");
    let u64_at = |o: usize| dll.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or("short PE");
    let pe = u32_at(0x3c)? as usize;
    if dll.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a PE file".into());
    }
    let nsec = u16_at(pe + 6)? as usize;
    let optsize = u16_at(pe + 20)? as usize;
    let opt = pe + 24;
    let base = if u16_at(opt)? == 0x20b { u64_at(opt + 24)? } else { u32_at(opt + 28)? as u64 };
    let mut secs = Vec::new();
    for i in 0..nsec {
        let o = opt + optsize + 40 * i;
        secs.push((u32_at(o + 12)? as u64, u32_at(o + 20)? as u64, u32_at(o + 16)? as u64)); // va, raw, raw size
    }
    let off2va = |off: u64| secs.iter().find(|(_, raw, rs)| *raw <= off && off < raw + rs).map(|(va, raw, _)| base + va + off - raw);
    let mut out = HashMap::new();
    let magic = b"\x05vorbis";
    let mut i = 0;
    while let Some(p) = dll[i..].windows(magic.len()).position(|w| w == magic) {
        let off = i + p;
        i = off + 1;
        let Some(va) = off2va(off as u64) else { continue };
        let needle = va.to_le_bytes();
        let Some(r) = dll.windows(8).position(|w| w == needle) else { continue };
        let (size, crc) = (u32_at(r + 8)? as usize, u32_at(r + 12)?);
        let Some(hdr) = dll.get(off..off + size) else { continue };
        if crc32fast::hash(hdr) == crc {
            out.insert(crc, hdr.to_vec());
        }
    }
    if out.is_empty() {
        return Err("no FMOD Vorbis setup table found in the player DLL".into());
    }
    Ok(out)
}

unsafe extern "C" {
    fn restrafe_vorbis_setup_header(ch: i32, rate: std::os::raw::c_long, q: f32, out: *mut u8, cap: i32) -> i32;
}

/// The setup header the vendored upstream libvorbis 1.3.4 writes for (channels, rate, quality): FMOD's encoder
/// produced the same bytes, so a stream's header is found by searching the quality for its CRC32.
pub fn libvorbis_setup(channels: u16, rate: u32, q: f32) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 1 << 16];
    let n = unsafe { restrafe_vorbis_setup_header(channels as i32, rate as _, q, buf.as_mut_ptr(), buf.len() as i32) };
    (n > 0).then(|| {
        buf.truncate(n as usize);
        buf
    })
}

/// Quality values tried for [`libvorbis_setup`] (FMOD maps its 1-100 quality onto libvorbis' -0.1 .. 1.0).
pub fn quality_grid() -> impl Iterator<Item = f32> {
    (-100..=1000).map(|i| i as f32 / 1000.0)
}

/// Setup headers: the FMOD table inside the player DLL, then libvorbis regeneration (quality per CRC remembered).
pub struct SetupSource {
    pub dll: HashMap<u32, Vec<u8>>,
    /// crc -> quality found by search (also loaded from / saved to a cache by the caller)
    pub qualities: std::sync::Mutex<HashMap<u32, f32>>,
}

impl SetupSource {
    pub fn get(&self, crc: u32, channels: u16, rate: u32) -> Option<Vec<u8>> {
        if let Some(h) = self.dll.get(&crc) {
            return Some(h.clone());
        }
        if let Some(&q) = self.qualities.lock().unwrap().get(&crc) {
            if let Some(h) = libvorbis_setup(channels, rate, q).filter(|h| crc32fast::hash(h) == crc) {
                return Some(h);
            }
        }
        let mut last = 0;
        for q in quality_grid() {
            let Some(h) = libvorbis_setup(channels, rate, q) else { continue };
            let c = crc32fast::hash(&h);
            if c == last {
                continue;
            }
            last = c;
            self.qualities.lock().unwrap().entry(c).or_insert(q);
            if c == crc {
                return Some(h);
            }
        }
        None
    }
}

#[derive(Debug)]
pub struct Pcm {
    pub channels: u16,
    pub rate: u32,
    /// interleaved
    pub samples: Vec<i16>,
}

const RATES: [u32; 11] = [4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000];

/// Decodes the first sample of an FSB5 bank (Unity writes one per AudioClip). Supports Vorbis (mode 15) and PCM16.
/// `setup(crc, channels, rate)` returns the Vorbis setup header with that CRC32 ([`SetupSource`]).
pub fn decode(fsb: &[u8], setup: &dyn Fn(u32, u16, u32) -> Option<Vec<u8>>) -> Result<Pcm> {
    let u32_at = |o: usize| fsb.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated FSB5".to_string());
    if fsb.get(..4) != Some(b"FSB5") {
        return Err("not an FSB5 bank".into());
    }
    let version = u32_at(4)?;
    let (_n, shdr_size, names_size, _data_size, mode) = (u32_at(8)?, u32_at(12)?, u32_at(16)?, u32_at(20)?, u32_at(24)?);
    let header = if version == 0 { 0x40 } else { 0x3c };
    let mut off = header;
    let raw = u64::from_le_bytes(fsb.get(off..off + 8).ok_or("truncated FSB5")?.try_into().unwrap());
    let mut next = raw & 1 != 0;
    let mut rate = RATES.get(((raw >> 1) & 0xf) as usize).copied().unwrap_or(44100);
    let mut channels = (((raw >> 5) & 1) + 1) as u16;
    let data_off = (((raw >> 7) & 0x7ff_ffff) * 32) as usize;
    let nsamples = (raw >> 34) as usize;
    off += 8;
    let mut crc = None;
    while next {
        let c = u32_at(off)?;
        next = c & 1 != 0;
        let size = ((c >> 1) & 0xff_ffff) as usize;
        let body = fsb.get(off + 4..off + 4 + size).ok_or("truncated FSB5 chunk")?;
        match c >> 25 {
            1 => channels = body[0] as u16,
            2 => rate = u32::from_le_bytes(body[..4].try_into().unwrap()),
            11 => crc = Some(u32::from_le_bytes(body[..4].try_into().unwrap())),
            _ => {}
        }
        off += 4 + size;
    }
    let data_start = header + shdr_size as usize + names_size as usize + data_off;
    let data = fsb.get(data_start..).ok_or("FSB5 sample data out of range")?;
    match mode {
        2 => {
            // PCM16
            let samples: Vec<i16> = data.chunks_exact(2).take(nsamples * channels as usize).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
            Ok(Pcm { channels, rate, samples })
        }
        15 => {
            let crc = crc.ok_or("FSB5 Vorbis stream without setup CRC")?;
            let setup = setup(crc, channels, rate).ok_or_else(|| format!("Vorbis setup header {crc:08x} not found ({channels} ch {rate} Hz)"))?;
            let mut ident = b"\x01vorbis".to_vec();
            ident.extend_from_slice(&0u32.to_le_bytes());
            ident.push(channels as u8);
            ident.extend_from_slice(&rate.to_le_bytes());
            ident.extend_from_slice(&[0u8; 12]); // bitrates
            ident.push((11 << 4) | 8); // FMOD blocksizes 256 / 2048
            ident.push(1);
            let ih = lewton::header::read_header_ident(&ident).map_err(|e| format!("ident: {e:?}"))?;
            let sh = lewton::header::read_header_setup(&setup, ih.audio_channels, (ih.blocksize_0, ih.blocksize_1)).map_err(|e| format!("setup: {e:?}"))?;
            let mut pwr = lewton::audio::PreviousWindowRight::new();
            let mut samples = Vec::with_capacity(nsamples * channels as usize);
            let mut p = 0;
            while p + 2 <= data.len() && samples.len() < nsamples * channels as usize {
                let len = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
                if len == 0 || p + 2 + len > data.len() {
                    break;
                }
                let pkt = &data[p + 2..p + 2 + len];
                p += 2 + len;
                let dec = lewton::audio::read_audio_packet(&ih, &sh, pkt, &mut pwr).map_err(|e| format!("packet: {e:?}"))?;
                if dec.is_empty() || dec[0].is_empty() {
                    continue;
                }
                for i in 0..dec[0].len() {
                    for ch in &dec {
                        samples.push(ch[i]);
                    }
                }
            }
            samples.truncate(nsamples * channels as usize);
            Ok(Pcm { channels, rate, samples })
        }
        m => Err(format!("FSB5 codec {m} not supported yet")),
    }
}

impl Pcm {
    /// A RIFF/WAVE file of the samples (16-bit PCM).
    pub fn wav(&self) -> Vec<u8> {
        let data_len = (self.samples.len() * 2) as u32;
        let mut v = Vec::with_capacity(44 + data_len as usize);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&self.channels.to_le_bytes());
        v.extend_from_slice(&self.rate.to_le_bytes());
        v.extend_from_slice(&(self.rate * self.channels as u32 * 2).to_le_bytes());
        v.extend_from_slice(&(self.channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }
}
