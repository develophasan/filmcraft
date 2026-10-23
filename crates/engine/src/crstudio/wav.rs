//! Reading WAV files of any common layout (RIFF chunks in any order, PCM 16/24/32-bit, IEEE float
//! 32/64-bit, WAVE_FORMAT_EXTENSIBLE) as mono `f32`, and writing mono float WAVs.

use std::path::Path;

/// Largest WAV read (bytes); longer narration files are refused instead of filling memory.
const MAX_WAV_BYTES: u64 = 512 * 1024 * 1024;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

/// Decoded mono audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Mono {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Mono {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.rate.max(1))
    }
}

/// Parse WAV bytes into mono samples (channels averaged).
pub fn parse(bytes: &[u8]) -> Result<Mono, String> {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("WAV değil (RIFF/WAVE başlığı yok)".into());
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, usize, u32, u16)> = None; // (format, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    let mut guard = 0;
    while pos + 8 <= bytes.len() && guard < 64 {
        guard += 1;
        let id = bytes.get(pos..pos + 4).unwrap_or(b"????");
        let size = u32_at(bytes, pos + 4).unwrap_or(0) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = bytes.get(body_start..body_end).unwrap_or(&[]);
        if id == b"fmt " {
            let mut tag = u16_at(body, 0).ok_or("fmt eksik")?;
            let ch = usize::from(u16_at(body, 2).ok_or("fmt eksik")?).max(1);
            let rate = u32_at(body, 4).ok_or("fmt eksik")?;
            let bits = u16_at(body, 14).ok_or("fmt eksik")?;
            if tag == 0xFFFE {
                // WAVE_FORMAT_EXTENSIBLE: the real format is the first two bytes of the sub-format GUID.
                tag = u16_at(body, 24).unwrap_or(1);
            }
            fmt = Some((tag, ch, rate, bits));
        } else if id == b"data" {
            data = Some(body);
        }
        pos = body_start.saturating_add(size).saturating_add(size & 1);
    }
    let (tag, ch, rate, bits) = fmt.ok_or("WAV fmt bölümü yok")?;
    let data = data.ok_or("WAV veri bölümü yok")?;
    if !(1000..=768_000).contains(&rate) {
        return Err(format!("Geçersiz örnekleme hızı: {rate}"));
    }
    let bps = usize::from(bits / 8).max(1);
    let frame = bps * ch;
    let read = |s: &[u8]| -> f32 {
        match (tag, bits) {
            (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (1, 8) => (s[0] as f32 - 128.0) / 128.0,
            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            (3, 64) => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
            _ => 0.0,
        }
    };
    if !matches!((tag, bits), (1, 8) | (1, 16) | (1, 24) | (1, 32) | (3, 32) | (3, 64)) {
        return Err(format!("Desteklenmeyen WAV biçimi (biçim {tag}, {bits} bit)"));
    }
    let mut samples = Vec::with_capacity(data.len() / frame.max(1));
    for f in data.chunks_exact(frame) {
        let mut acc = 0.0f32;
        for c in f.chunks_exact(bps) {
            if c.len() == bps {
                acc += read(c);
            }
        }
        let v = acc / ch as f32;
        samples.push(if v.is_finite() { v } else { 0.0 });
    }
    Ok(Mono { samples, rate })
}

/// Read a WAV file as mono.
pub fn read(path: &Path) -> Result<Mono, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_WAV_BYTES {
        return Err(format!("{}: dosya çok büyük", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes)
}

/// Duration of a WAV file in seconds.
pub fn duration(path: &Path) -> Option<f64> {
    read(path).ok().map(|m| m.seconds()).filter(|d| d.is_finite() && *d > 0.0)
}

/// Mono 32-bit float WAV bytes.
pub fn encode(samples: &[f32], rate: u32) -> Vec<u8> {
    crate::voiceover::write_wav_f32(samples, rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav16(extra_chunk: bool) -> Vec<u8> {
        let samples: Vec<i16> = (0..480).map(|i| ((i as f32 * 0.1).sin() * 16000.0) as i16).collect();
        let mut data = Vec::new();
        for s in &samples {
            data.extend_from_slice(&s.to_le_bytes());
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        if extra_chunk {
            b.extend_from_slice(b"LIST");
            b.extend_from_slice(&5u32.to_le_bytes());
            b.extend_from_slice(b"abcde\0");
        }
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&48000u32.to_le_bytes());
        b.extend_from_slice(&(48000u32 * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn reads_chunks_in_any_order() {
        let a = parse(&wav16(false)).unwrap();
        let b = parse(&wav16(true)).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.samples.len(), 480);
        assert!((a.seconds() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn float_round_trip_and_hostile_input() {
        let m = parse(&encode(&[0.5, -0.25, 0.0], 24000)).unwrap();
        assert_eq!(m.rate, 24000);
        assert_eq!(m.samples, vec![0.5, -0.25, 0.0]);
        assert!(parse(b"RIFF").is_err());
        let mut bad = wav16(false);
        bad.truncate(50);
        let _ = parse(&bad);
    }
}
