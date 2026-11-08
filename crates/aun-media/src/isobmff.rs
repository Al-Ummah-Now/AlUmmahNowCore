//! A small ISO BMFF (MP4/MOV/M4A) metadata parser: enough for tracks, codecs, durations and
//! keyframes. Input is untrusted: sizes are bounds-checked and nesting is limited.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use aun_common::{Error, Result};

const MAX_DEPTH: u32 = 12;
/// Largest sample table read into memory (about 8M entries).
const MAX_TABLE_BYTES: u64 = 64 * 1024 * 1024;
/// Samples walked when building a keyframe index, so a corrupt `stts` cannot spin for hours.
const MAX_SAMPLES: u64 = 50_000_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct Movie {
    pub brand: String,
    pub compatible: Vec<String>,
    pub timescale: u32,
    pub duration: u64,
    pub tracks: Vec<Trak>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Trak {
    pub id: u32,
    pub handler: String,
    pub language: Option<String>,
    pub timescale: u32,
    pub duration: u64,
    pub sample_entry: String,
    pub width: u32,
    pub height: u32,
    pub channels: u32,
    pub sample_rate: u32,
    /// `(sample_count, sample_delta)` pairs from `stts`.
    pub stts: Vec<(u32, u32)>,
    /// 1-based sync sample numbers; `None` means every sample is a sync sample.
    pub stss: Option<Vec<u32>>,
}

impl Trak {
    pub fn sample_count(&self) -> u64 {
        self.stts.iter().map(|(n, _)| u64::from(*n)).sum()
    }

    pub fn stts_duration(&self) -> u64 {
        self.stts
            .iter()
            .fold(0u64, |t, (n, d)| t.saturating_add(u64::from(*n) * u64::from(*d)))
    }

    pub fn duration_ms(&self) -> Option<u64> {
        let units = if self.duration > 0 && self.duration != u64::MAX {
            self.duration
        } else {
            self.stts_duration()
        };
        (units > 0 && self.timescale > 0).then(|| units_to_ms(units, self.timescale))
    }

    pub fn fps(&self) -> Option<f64> {
        let total = self.stts_duration();
        (total > 0 && self.timescale > 0)
            .then(|| self.sample_count() as f64 * f64::from(self.timescale) / total as f64)
    }

    /// Presentation-order independent keyframe times (decode timestamps) in milliseconds.
    pub fn keyframes_ms(&self) -> Vec<u64> {
        let mut out = Vec::new();
        let mut next_sync = self.stss.as_ref().map(|s| s.iter().copied().peekable());
        let mut sample: u64 = 1;
        let mut time: u64 = 0;
        for (count, delta) in &self.stts {
            for _ in 0..*count {
                if sample > MAX_SAMPLES {
                    return out;
                }
                let is_sync = match &mut next_sync {
                    None => true,
                    Some(iter) => {
                        while iter.peek().is_some_and(|s| u64::from(*s) < sample) {
                            iter.next();
                        }
                        iter.peek().is_some_and(|s| u64::from(*s) == sample)
                    }
                };
                if is_sync && self.timescale > 0 {
                    out.push(units_to_ms(time, self.timescale));
                }
                sample += 1;
                time = time.saturating_add(u64::from(*delta));
            }
        }
        out
    }
}

pub(crate) fn units_to_ms(units: u64, timescale: u32) -> u64 {
    ((u128::from(units) * 1000) / u128::from(timescale.max(1))) as u64
}

/// Does this file start with an `ftyp` box?
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.len() >= 8 && &head[4..8] == b"ftyp"
}

pub(crate) fn parse_file(path: &Path) -> Result<Movie> {
    let file = File::open(path).map_err(|e| crate::audio::open_error(path, e))?;
    let size = file.metadata()?.len();
    let mut parser = Parser {
        reader: BufReader::new(file),
        movie: Movie::default(),
    };
    parser.walk(0, size, 0, Scope::Root)?;
    if parser.movie.timescale == 0 && parser.movie.tracks.is_empty() {
        return Err(Error::InvalidInput(format!("no moov box in {}", path.display())));
    }
    Ok(parser.movie)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Root,
    Moov,
    Trak,
    Mdia,
    Minf,
    Stbl,
}

struct Parser {
    reader: BufReader<File>,
    movie: Movie,
}

fn malformed(what: &str) -> Error {
    Error::InvalidInput(format!("malformed MP4: {what}"))
}

impl Parser {
    fn walk(&mut self, start: u64, end: u64, depth: u32, scope: Scope) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(malformed("boxes nested too deep"));
        }
        let mut pos = start;
        while pos + 8 <= end {
            self.reader.seek(SeekFrom::Start(pos))?;
            let mut head = [0u8; 8];
            self.reader.read_exact(&mut head)?;
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let kind = [head[4], head[5], head[6], head[7]];
            let mut header_len = 8;
            if size == 1 {
                let mut large = [0u8; 8];
                self.reader.read_exact(&mut large)?;
                size = u64::from_be_bytes(large);
                header_len = 16;
            } else if size == 0 {
                size = end - pos;
            }
            if size < header_len || pos.checked_add(size).is_none_or(|e| e > end) {
                // A truncated trailing box (for example an interrupted mdat) ends the walk.
                if scope == Scope::Root && self.movie.timescale != 0 {
                    return Ok(());
                }
                return Err(malformed("box size out of range"));
            }
            let body = pos + header_len;
            let box_end = pos + size;
            self.handle(&kind, body, box_end, depth, scope)?;
            pos = box_end;
        }
        Ok(())
    }

    fn handle(&mut self, kind: &[u8; 4], body: u64, end: u64, depth: u32, scope: Scope) -> Result<()> {
        match (scope, kind) {
            (Scope::Root, b"ftyp") => {
                let data = self.payload(body, end)?;
                if data.len() >= 8 {
                    self.movie.brand = fourcc(&data[0..4]);
                    self.movie.compatible = data[8..].as_chunks::<4>().0.iter().map(|c| fourcc(c)).collect();
                }
            }
            (Scope::Root, b"moov") => self.walk(body, end, depth + 1, Scope::Moov)?,
            (Scope::Moov, b"mvhd") => {
                let data = self.payload(body, end)?;
                let (timescale, duration) = version_times(&data)?;
                self.movie.timescale = timescale;
                self.movie.duration = duration;
            }
            (Scope::Moov, b"trak") => {
                self.movie.tracks.push(Trak::default());
                self.walk(body, end, depth + 1, Scope::Trak)?;
            }
            (Scope::Trak, b"tkhd") => {
                let data = self.payload(body, end)?;
                let version = *data.first().ok_or_else(|| malformed("empty tkhd"))?;
                let id_at = if version == 1 { 20 } else { 12 };
                let id = be32(&data, id_at)?;
                let dims_at = if version == 1 { 88 } else { 76 };
                let (w, h) = (be32(&data, dims_at)? >> 16, be32(&data, dims_at + 4)? >> 16);
                let t = self.track()?;
                t.id = id;
                t.width = w;
                t.height = h;
            }
            (Scope::Trak, b"mdia") => self.walk(body, end, depth + 1, Scope::Mdia)?,
            (Scope::Mdia, b"mdhd") => {
                let data = self.payload(body, end)?;
                let (timescale, duration) = version_times(&data)?;
                let lang_at = if data.first() == Some(&1) { 32 } else { 20 };
                let code = be16(&data, lang_at)?;
                let t = self.track()?;
                t.timescale = timescale;
                t.duration = duration;
                t.language = decode_language(code);
            }
            (Scope::Mdia, b"hdlr") => {
                let data = self.payload(body, end)?;
                if data.len() < 12 {
                    return Err(malformed("short hdlr"));
                }
                let handler = fourcc(&data[8..12]);
                self.track()?.handler = handler;
            }
            (Scope::Mdia, b"minf") => self.walk(body, end, depth + 1, Scope::Minf)?,
            (Scope::Minf, b"stbl") => self.walk(body, end, depth + 1, Scope::Stbl)?,
            (Scope::Stbl, b"stsd") => {
                let data = self.payload(body, end)?;
                self.read_stsd(&data)?;
            }
            (Scope::Stbl, b"stts") => {
                let data = self.payload(body, end)?;
                let count = be32(&data, 4)? as usize;
                if count.saturating_mul(8) > data.len().saturating_sub(8) {
                    return Err(malformed("stts entry count"));
                }
                let mut entries = Vec::with_capacity(count);
                for i in 0..count {
                    entries.push((be32(&data, 8 + i * 8)?, be32(&data, 12 + i * 8)?));
                }
                self.track()?.stts = entries;
            }
            (Scope::Stbl, b"stss") => {
                let data = self.payload(body, end)?;
                let count = be32(&data, 4)? as usize;
                if count.saturating_mul(4) > data.len().saturating_sub(8) {
                    return Err(malformed("stss entry count"));
                }
                let mut entries = Vec::with_capacity(count);
                for i in 0..count {
                    entries.push(be32(&data, 8 + i * 4)?);
                }
                self.track()?.stss = Some(entries);
            }
            _ => {}
        }
        Ok(())
    }

    fn read_stsd(&mut self, data: &[u8]) -> Result<()> {
        if be32(data, 4)? == 0 || data.len() < 16 {
            return Ok(());
        }
        let entry = &data[8..];
        let handler = self.track()?.handler.clone();
        let sample_entry = fourcc(&entry[4..8]);
        let (mut w, mut h, mut channels, mut rate) = (0, 0, 0, 0);
        match handler.as_str() {
            "vide" => {
                w = u32::from(be16(entry, 32)?);
                h = u32::from(be16(entry, 34)?);
            }
            "soun" => {
                channels = u32::from(be16(entry, 24)?);
                rate = be32(entry, 32)? >> 16;
            }
            _ => {}
        }
        let t = self.track()?;
        t.sample_entry = sample_entry;
        if w > 0 && h > 0 {
            t.width = w;
            t.height = h;
        }
        t.channels = channels;
        t.sample_rate = rate;
        Ok(())
    }

    fn track(&mut self) -> Result<&mut Trak> {
        self.movie
            .tracks
            .last_mut()
            .ok_or_else(|| malformed("box outside trak"))
    }

    fn payload(&mut self, body: u64, end: u64) -> Result<Vec<u8>> {
        let len = end.saturating_sub(body);
        if len > MAX_TABLE_BYTES {
            return Err(malformed("metadata box too large"));
        }
        self.reader.seek(SeekFrom::Start(body))?;
        let mut data = vec![0u8; len as usize];
        self.reader.read_exact(&mut data)?;
        Ok(data)
    }
}

fn fourcc(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}

fn be16(data: &[u8], at: usize) -> Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| malformed("short box"))
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| malformed("short box"))
}

fn be64(data: &[u8], at: usize) -> Result<u64> {
    Ok((u64::from(be32(data, at)?) << 32) | u64::from(be32(data, at + 4)?))
}

/// `(timescale, duration)` from an `mvhd`/`mdhd` payload.
fn version_times(data: &[u8]) -> Result<(u32, u64)> {
    match data.first() {
        Some(1) => Ok((be32(data, 20)?, be64(data, 24)?)),
        Some(_) => {
            let d = be32(data, 16)?;
            Ok((be32(data, 12)?, if d == u32::MAX { 0 } else { u64::from(d) }))
        }
        None => Err(malformed("empty header box")),
    }
}

fn decode_language(code: u16) -> Option<String> {
    if code == 0 || code == 0x55C4 {
        return None;
    }
    let letters = [(code >> 10) & 31, (code >> 5) & 31, code & 31];
    letters
        .iter()
        .map(|l| (1..=26).contains(l).then(|| char::from(*l as u8 + 0x60)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_decoding() {
        // ISO 639-2 "ara", packed 5 bits per letter.
        let code =
            ((u16::from(b'a') - 0x60) << 10) | ((u16::from(b'r') - 0x60) << 5) | (u16::from(b'a') - 0x60);
        assert_eq!(decode_language(code).as_deref(), Some("ara"));
        assert_eq!(decode_language(0x55C4), None);
    }

    #[test]
    fn keyframes_from_tables() {
        let t = Trak {
            timescale: 30,
            stts: vec![(90, 1)],
            stss: Some(vec![1, 31, 61]),
            ..Default::default()
        };
        assert_eq!(t.keyframes_ms(), vec![0, 1000, 2000]);
        assert!((t.fps().unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn no_stss_means_all_sync() {
        let t = Trak {
            timescale: 2,
            stts: vec![(3, 1)],
            stss: None,
            ..Default::default()
        };
        assert_eq!(t.keyframes_ms(), vec![0, 500, 1000]);
    }
}
