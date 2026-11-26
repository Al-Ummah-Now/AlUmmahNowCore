//! Progressive MP4 → fMP4 HLS, without re-encoding. The input is what the platform encoder wrote
//! for one rung (H.264 + optional AAC, a keyframe every [`super::GOP_SECONDS`]). Segments are cut at
//! the first video keyframe at or past each multiple of `segment_seconds`, like FFmpeg's `hls_time`,
//! so the layout matches the server's encode. Input is bounds-checked; only the first video and the
//! first audio track are kept.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Largest `moov` read into memory.
const MAX_MOOV_BYTES: u64 = 256 * 1024 * 1024;
/// Largest single sample (a 1080p keyframe is far below this).
const MAX_SAMPLE_BYTES: u32 = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 20_000_000;

/// One file written for a rung, relative to its folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HlsPackage {
    /// `index.m3u8`, `init.mp4` and every segment, in that order.
    pub files: Vec<PackagedFile>,
    /// Segment lengths in seconds, as written to the playlist.
    pub segment_seconds: Vec<f64>,
    pub duration_ms: u64,
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::invalid_input(format!("mp4: {}", msg.into()))
}

// ---- reading -------------------------------------------------------------------------------------

/// One box inside a buffer: its type, the whole box and its payload.
struct BoxRef<'a> {
    kind: [u8; 4],
    whole: &'a [u8],
    payload: &'a [u8],
}

fn children(buf: &[u8]) -> Result<Vec<BoxRef<'_>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < buf.len() {
        if buf.len() - at < 8 {
            return Err(invalid("truncated box header"));
        }
        let size32 = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
        let kind = [buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]];
        let (size, header) = match size32 {
            0 => ((buf.len() - at) as u64, 8usize),
            1 => {
                if buf.len() - at < 16 {
                    return Err(invalid("truncated large box"));
                }
                (
                    u64::from_be_bytes(buf[at + 8..at + 16].try_into().map_err(|_| invalid("box size"))?),
                    16,
                )
            }
            n => (u64::from(n), 8),
        };
        if size < header as u64 || size > (buf.len() - at) as u64 {
            return Err(invalid(format!(
                "box {} overruns its parent",
                String::from_utf8_lossy(&kind)
            )));
        }
        let end = at + size as usize;
        out.push(BoxRef {
            kind,
            whole: &buf[at..end],
            payload: &buf[at + header..end],
        });
        at = end;
    }
    Ok(out)
}

fn child<'a>(boxes: &'a [BoxRef<'a>], kind: &[u8; 4]) -> Option<&'a BoxRef<'a>> {
    boxes.iter().find(|b| &b.kind == kind)
}

fn need<'a>(boxes: &'a [BoxRef<'a>], kind: &[u8; 4]) -> Result<&'a BoxRef<'a>> {
    child(boxes, kind).ok_or_else(|| invalid(format!("missing {}", String::from_utf8_lossy(kind))))
}

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| invalid("truncated table"))?;
        let s = &self.buf[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| invalid("u32"))?,
        ))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| invalid("u64"))?,
        ))
    }
    /// Version and flags of a full box.
    fn full(&mut self) -> Result<(u8, u32)> {
        let v = self.u8()?;
        let f = self.take(3)?;
        Ok((
            v,
            (u32::from(f[0]) << 16) | (u32::from(f[1]) << 8) | u32::from(f[2]),
        ))
    }
    /// An entry count, checked against the bytes left so a corrupt count cannot allocate wildly.
    fn count(&mut self, entry_bytes: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > MAX_SAMPLES || n.saturating_mul(entry_bytes) > self.buf.len() - self.at {
            return Err(invalid("table count exceeds its box"));
        }
        Ok(n)
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    offset: u64,
    size: u32,
    dts: u64,
    duration: u32,
    cts: i64,
    sync: bool,
}

#[derive(Debug)]
struct Track {
    id: u32,
    video: bool,
    timescale: u32,
    tkhd: Vec<u8>,
    edts: Option<Vec<u8>>,
    mdhd: Vec<u8>,
    hdlr: Vec<u8>,
    media_header: Vec<u8>,
    dinf: Option<Vec<u8>>,
    stsd: Vec<u8>,
    samples: Vec<Sample>,
}

impl Track {
    fn end(&self) -> u64 {
        self.samples.last().map_or(0, |s| s.dts + u64::from(s.duration))
    }
}

/// Finds and reads `moov`, refusing fragmented files (a progressive MP4 is expected).
fn read_moov(file: &mut File) -> Result<Vec<u8>> {
    let len = file.metadata()?.len();
    let mut at = 0u64;
    let mut moov = None;
    while at + 8 <= len {
        file.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 16];
        file.read_exact(&mut head[..8])?;
        let kind = [head[4], head[5], head[6], head[7]];
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let (size, header) = match size32 {
            0 => (len - at, 8u64),
            1 => {
                file.read_exact(&mut head[8..16])?;
                (
                    u64::from_be_bytes(head[8..16].try_into().map_err(|_| invalid("box size"))?),
                    16,
                )
            }
            n => (u64::from(n), 8),
        };
        if size < header || at + size > len {
            return Err(invalid("top-level box overruns the file"));
        }
        match &kind {
            b"moof" => {
                return Err(Error::Unsupported(
                    "fragmented MP4 input: write a progressive MP4".to_owned(),
                ));
            }
            b"moov" => {
                if size - header > MAX_MOOV_BYTES {
                    return Err(invalid("moov too large"));
                }
                let mut buf = vec![0u8; (size - header) as usize];
                file.read_exact(&mut buf)?;
                moov = Some(buf);
            }
            _ => {}
        }
        at += size;
    }
    moov.ok_or_else(|| invalid("no moov box"))
}

fn timescale_of(mdhd: &[u8]) -> Result<u32> {
    let mut c = Cursor::new(mdhd);
    let (v, _) = c.full()?;
    c.take(if v == 1 { 16 } else { 8 })?;
    let ts = c.u32()?;
    if ts == 0 {
        return Err(invalid("zero timescale"));
    }
    Ok(ts)
}

fn track_id_of(tkhd: &[u8]) -> Result<u32> {
    let mut c = Cursor::new(tkhd);
    let (v, _) = c.full()?;
    c.take(if v == 1 { 16 } else { 8 })?;
    c.u32()
}

fn parse_track(trak: &[u8]) -> Result<Option<Track>> {
    let boxes = children(trak)?;
    let tkhd = need(&boxes, b"tkhd")?;
    let mdia_boxes = children(need(&boxes, b"mdia")?.payload)?;
    let mdhd = need(&mdia_boxes, b"mdhd")?;
    let hdlr = need(&mdia_boxes, b"hdlr")?;
    let handler = hdlr.payload.get(8..12).ok_or_else(|| invalid("short hdlr"))?;
    let video = match handler {
        b"vide" => true,
        b"soun" => false,
        _ => return Ok(None),
    };
    let minf_boxes = children(need(&mdia_boxes, b"minf")?.payload)?;
    let media_header = need(&minf_boxes, if video { b"vmhd" } else { b"smhd" })?;
    let stbl = children(need(&minf_boxes, b"stbl")?.payload)?;
    let stsd = need(&stbl, b"stsd")?;
    let samples = read_samples(&stbl)?;
    Ok(Some(Track {
        id: track_id_of(tkhd.payload)?,
        video,
        timescale: timescale_of(mdhd.payload)?,
        tkhd: tkhd.whole.to_vec(),
        edts: child(&boxes, b"edts").map(|b| b.whole.to_vec()),
        mdhd: mdhd.whole.to_vec(),
        hdlr: hdlr.whole.to_vec(),
        media_header: media_header.whole.to_vec(),
        dinf: child(&minf_boxes, b"dinf").map(|b| b.whole.to_vec()),
        stsd: stsd.whole.to_vec(),
        samples,
    }))
}

fn read_samples(stbl: &[BoxRef<'_>]) -> Result<Vec<Sample>> {
    // Sizes
    let stsz = child(stbl, b"stsz").ok_or_else(|| {
        if child(stbl, b"stz2").is_some() {
            Error::Unsupported("compact sample sizes (stz2)".to_owned())
        } else {
            invalid("missing stsz")
        }
    })?;
    let mut c = Cursor::new(stsz.payload);
    c.full()?;
    let fixed = c.u32()?;
    let sizes: Vec<u32> = if fixed != 0 {
        let n = c.u32()? as usize;
        if n > MAX_SAMPLES {
            return Err(invalid("too many samples"));
        }
        vec![fixed; n]
    } else {
        let n = c.count(4)?;
        (0..n).map(|_| c.u32()).collect::<Result<_>>()?
    };
    let n = sizes.len();
    if n == 0 {
        return Err(invalid("track without samples"));
    }
    if let Some(big) = sizes.iter().find(|s| **s > MAX_SAMPLE_BYTES) {
        return Err(invalid(format!("sample of {big} bytes")));
    }
    // Decode times
    let mut c = Cursor::new(need(stbl, b"stts")?.payload);
    c.full()?;
    let entries = c.count(8)?;
    let mut durations = Vec::with_capacity(n);
    for _ in 0..entries {
        let (count, delta) = (c.u32()? as usize, c.u32()?);
        if durations.len() + count > n {
            return Err(invalid("stts covers more samples than stsz"));
        }
        durations.extend(std::iter::repeat_n(delta, count));
    }
    if durations.len() != n {
        return Err(invalid("stts and stsz disagree"));
    }
    // Composition offsets
    let mut cts = vec![0i64; n];
    if let Some(ctts) = child(stbl, b"ctts") {
        let mut c = Cursor::new(ctts.payload);
        let (version, _) = c.full()?;
        let entries = c.count(8)?;
        let mut i = 0usize;
        for _ in 0..entries {
            let count = c.u32()? as usize;
            let raw = c.u32()?;
            let off = if version == 1 {
                i64::from(raw as i32)
            } else {
                i64::from(raw)
            };
            if i + count > n {
                return Err(invalid("ctts covers more samples than stsz"));
            }
            cts[i..i + count].fill(off);
            i += count;
        }
    }
    // Sync samples (absent: every sample is one)
    let mut sync = vec![child(stbl, b"stss").is_none(); n];
    if let Some(stss) = child(stbl, b"stss") {
        let mut c = Cursor::new(stss.payload);
        c.full()?;
        for _ in 0..c.count(4)? {
            let s = c.u32()? as usize;
            if s == 0 || s > n {
                return Err(invalid("stss names a sample that does not exist"));
            }
            sync[s - 1] = true;
        }
    }
    // Chunk offsets
    let offsets: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        let mut c = Cursor::new(stco.payload);
        c.full()?;
        (0..c.count(4)?)
            .map(|_| c.u32().map(u64::from))
            .collect::<Result<_>>()?
    } else {
        let mut c = Cursor::new(need(stbl, b"co64")?.payload);
        c.full()?;
        (0..c.count(8)?).map(|_| c.u64()).collect::<Result<_>>()?
    };
    // Samples per chunk
    let mut c = Cursor::new(need(stbl, b"stsc")?.payload);
    c.full()?;
    let entries = c.count(12)?;
    let mut runs = Vec::with_capacity(entries);
    for _ in 0..entries {
        let (first, per, sdi) = (c.u32()?, c.u32()?, c.u32()?);
        if sdi != 1 {
            return Err(Error::Unsupported("more than one sample description".to_owned()));
        }
        if first == 0 || runs.last().is_some_and(|(f, _)| *f >= first) {
            return Err(invalid("stsc out of order"));
        }
        runs.push((first, per));
    }
    let mut out = Vec::with_capacity(n);
    let mut sample = 0usize;
    let mut dts = 0u64;
    for (ci, chunk_offset) in offsets.iter().enumerate() {
        let chunk = ci as u32 + 1;
        let per = runs
            .iter()
            .rev()
            .find(|(f, _)| *f <= chunk)
            .map_or(0, |(_, p)| *p) as usize;
        let mut offset = *chunk_offset;
        for _ in 0..per {
            if sample >= n {
                return Err(invalid("chunks hold more samples than stsz"));
            }
            out.push(Sample {
                offset,
                size: sizes[sample],
                dts,
                duration: durations[sample],
                cts: cts[sample],
                sync: sync[sample],
            });
            offset += u64::from(sizes[sample]);
            dts += u64::from(durations[sample]);
            sample += 1;
        }
    }
    if out.len() != n {
        return Err(invalid("chunks hold fewer samples than stsz"));
    }
    Ok(out)
}

// ---- writing -------------------------------------------------------------------------------------

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(body.len() + 4);
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(body);
    bx(kind, &p)
}

/// A copy of a full box (mvhd, tkhd or mdhd) with its duration set to zero, as fragmented files
/// carry their timing in the fragments (version 1 boxes have 64-bit times and duration).
fn zero_duration(whole: &[u8], kind: &[u8; 4]) -> Vec<u8> {
    let mut out = whole.to_vec();
    let Some(&version) = out.get(8) else { return out };
    let (at, len) = match (kind, version) {
        (b"mvhd", 1) => (8 + 4 + 8 + 8 + 4, 8),
        (b"mvhd", _) => (8 + 4 + 4 + 4 + 4, 4),
        (b"tkhd", 1) => (8 + 4 + 8 + 8 + 4 + 4, 8),
        (b"tkhd", _) => (8 + 4 + 4 + 4 + 4 + 4, 4),
        (b"mdhd", 1) => (8 + 4 + 8 + 8 + 4, 8),
        (b"mdhd", _) => (8 + 4 + 4 + 4 + 4, 4),
        _ => return out,
    };
    if let Some(field) = out.get_mut(at..at + len) {
        field.fill(0);
    }
    out
}

fn init_segment(mvhd: &[u8], tracks: &[&Track]) -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso6");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    for brand in [b"iso6", b"iso5", b"mp41"] {
        ftyp.extend_from_slice(brand);
    }
    let mut moov = zero_duration(mvhd, b"mvhd");
    let mut mvex = Vec::new();
    for t in tracks {
        let empty = |kind: &[u8; 4]| full(kind, 0, 0, &0u32.to_be_bytes());
        let mut stbl = t.stsd.clone();
        stbl.extend(empty(b"stts"));
        stbl.extend(empty(b"stsc"));
        stbl.extend(full(b"stsz", 0, 0, &[0u8; 8]));
        stbl.extend(empty(b"stco"));
        let mut minf = t.media_header.clone();
        minf.extend(t.dinf.clone().unwrap_or_else(|| {
            // dref with one self-contained url entry
            let url = full(b"url ", 0, 1, &[]);
            let mut dref_body = 1u32.to_be_bytes().to_vec();
            dref_body.extend(url);
            bx(b"dinf", &full(b"dref", 0, 0, &dref_body))
        }));
        minf.extend(bx(b"stbl", &stbl));
        let mut mdia = zero_duration(&t.mdhd, b"mdhd");
        mdia.extend_from_slice(&t.hdlr);
        mdia.extend(bx(b"minf", &minf));
        let mut trak = zero_duration(&t.tkhd, b"tkhd");
        if let Some(edts) = &t.edts {
            trak.extend_from_slice(edts);
        }
        trak.extend(bx(b"mdia", &mdia));
        moov.extend(bx(b"trak", &trak));
        let mut trex = t.id.to_be_bytes().to_vec();
        for v in [1u32, 0, 0, 0] {
            trex.extend_from_slice(&v.to_be_bytes());
        }
        mvex.extend(full(b"trex", 0, 0, &trex));
    }
    moov.extend(bx(b"mvex", &mvex));
    let mut out = bx(b"ftyp", &ftyp);
    out.extend(bx(b"moov", &moov));
    out
}

const SYNC_FLAGS: u32 = 0x0200_0000;
const NON_SYNC_FLAGS: u32 = 0x0101_0000;

/// One `moof` for the given runs of samples. `data_offsets` are filled in by the caller.
fn moof(sequence: u32, runs: &[(&Track, &[Sample])], data_offsets: &[i32]) -> Vec<u8> {
    let mut body = full(b"mfhd", 0, 0, &sequence.to_be_bytes());
    for (i, (track, samples)) in runs.iter().enumerate() {
        let tfhd = full(b"tfhd", 0, 0x02_0000, &track.id.to_be_bytes());
        let tfdt = full(b"tfdt", 1, 0, &samples[0].dts.to_be_bytes());
        let mut trun = (samples.len() as u32).to_be_bytes().to_vec();
        trun.extend_from_slice(&data_offsets[i].to_be_bytes());
        for s in *samples {
            trun.extend_from_slice(&s.duration.to_be_bytes());
            trun.extend_from_slice(&s.size.to_be_bytes());
            trun.extend_from_slice(&(if s.sync { SYNC_FLAGS } else { NON_SYNC_FLAGS }).to_be_bytes());
            trun.extend_from_slice(
                &(s.cts.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32).to_be_bytes(),
            );
        }
        let trun = full(b"trun", 1, 0x0001 | 0x0100 | 0x0200 | 0x0400 | 0x0800, &trun);
        let mut traf = tfhd;
        traf.extend(tfdt);
        traf.extend(trun);
        body.extend(bx(b"traf", &traf));
    }
    bx(b"moof", &body)
}

fn copy_samples(input: &mut File, samples: &[Sample], out: &mut impl Write) -> Result<()> {
    let mut buf = Vec::new();
    for s in samples {
        buf.resize(s.size as usize, 0);
        input.seek(SeekFrom::Start(s.offset))?;
        input
            .read_exact(&mut buf)
            .map_err(|_| invalid("sample data lies outside the file"))?;
        out.write_all(&buf)?;
    }
    Ok(())
}

fn write_file(dir: &Path, name: &str, bytes: &[u8], files: &mut Vec<PackagedFile>) -> Result<()> {
    fs::write(dir.join(name), bytes)?;
    files.push(PackagedFile {
        path: name.to_owned(),
        bytes: bytes.len() as u64,
    });
    Ok(())
}

/// Packages `input` (a progressive MP4) into `out_dir` as fMP4 HLS: `init.mp4`, `seg_00001.m4s`…
/// and `index.m3u8`. Blocking; call it on a worker thread.
pub fn package_mp4(
    input: &Path,
    out_dir: &Path,
    segment_seconds: u32,
    cancel: &CancellationToken,
) -> Result<HlsPackage> {
    if segment_seconds == 0 {
        return Err(Error::invalid_input("segment length must be positive".to_owned()));
    }
    let mut file = File::open(input).map_err(|e| crate::audio::open_error(input, e))?;
    let moov = read_moov(&mut file)?;
    let boxes = children(&moov)?;
    if child(&boxes, b"mvex").is_some() {
        return Err(Error::Unsupported(
            "fragmented MP4 input: write a progressive MP4".to_owned(),
        ));
    }
    let mvhd = need(&boxes, b"mvhd")?;
    let mut video = None;
    let mut audio = None;
    for trak in boxes.iter().filter(|b| &b.kind == b"trak") {
        if let Some(t) = parse_track(trak.payload)? {
            if t.video && video.is_none() {
                video = Some(t);
            } else if !t.video && audio.is_none() {
                audio = Some(t);
            }
        }
    }
    let video = video.ok_or_else(|| invalid("no video track"))?;
    if !video.samples[0].sync {
        return Err(invalid("the video does not start on a keyframe"));
    }
    fs::create_dir_all(out_dir)?;

    // Cut points: indices of the video samples that start each segment
    let vts = u64::from(video.timescale);
    let step = u64::from(segment_seconds) * vts;
    let pts0 = video
        .samples
        .iter()
        .map(|s| s.dts as i64 + s.cts)
        .min()
        .unwrap_or(0);
    let mut cuts = vec![0usize];
    let mut next = step;
    for (i, s) in video.samples.iter().enumerate().skip(1) {
        let pts = (s.dts as i64 + s.cts - pts0).max(0) as u64;
        if s.sync && pts >= next {
            cuts.push(i);
            next = (pts / step + 1) * step;
        }
    }

    let tracks: Vec<&Track> = std::iter::once(&video).chain(audio.as_ref()).collect();
    let mut files = Vec::new();
    write_file(
        out_dir,
        "init.mp4",
        &init_segment(mvhd.whole, &tracks),
        &mut files,
    )?;

    let mut segment_seconds_out = Vec::with_capacity(cuts.len());
    let mut audio_from = 0usize;
    for (k, &start) in cuts.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let end = cuts.get(k + 1).copied().unwrap_or(video.samples.len());
        let v = &video.samples[start..end];
        let seg_start_dts = v[0].dts;
        let seg_end_dts = video.samples.get(end).map_or(video.end(), |s| s.dts);
        let mut runs: Vec<(&Track, &[Sample])> = vec![(&video, v)];
        if let Some(a) = &audio {
            // Audio up to the next cut's time (the rest for the last segment)
            let audio_to = if end == video.samples.len() {
                a.samples.len()
            } else {
                let limit = u128::from(seg_end_dts) * u128::from(a.timescale);
                audio_from
                    + a.samples[audio_from..]
                        .iter()
                        .take_while(|s| u128::from(s.dts) * u128::from(vts) < limit)
                        .count()
            };
            if audio_to > audio_from {
                runs.push((a, &a.samples[audio_from..audio_to]));
            }
            audio_from = audio_to;
        }
        let placeholder = vec![0i32; runs.len()];
        let moof_len = moof(k as u32 + 1, &runs, &placeholder).len();
        let mut offsets = Vec::with_capacity(runs.len());
        let mut at = moof_len as u64 + 8;
        let mut mdat_len = 0u64;
        for (_, samples) in &runs {
            offsets.push(i32::try_from(at).map_err(|_| invalid("segment too large"))?);
            let bytes: u64 = samples.iter().map(|s| u64::from(s.size)).sum();
            at += bytes;
            mdat_len += bytes;
        }
        let name = format!("seg_{:05}.m4s", k + 1);
        let path = out_dir.join(&name);
        let mut out = BufWriter::new(File::create(&path)?);
        let mut styp = b"msdh".to_vec();
        styp.extend_from_slice(&0u32.to_be_bytes());
        styp.extend_from_slice(b"msdhmsix");
        out.write_all(&bx(b"styp", &styp))?;
        out.write_all(&moof(k as u32 + 1, &runs, &offsets))?;
        let mdat_size = u32::try_from(mdat_len + 8).map_err(|_| invalid("segment too large"))?;
        out.write_all(&mdat_size.to_be_bytes())?;
        out.write_all(b"mdat")?;
        for (_, samples) in &runs {
            copy_samples(&mut file, samples, &mut out)?;
        }
        out.flush()?;
        drop(out);
        files.push(PackagedFile {
            path: name,
            bytes: fs::metadata(&path)?.len(),
        });
        segment_seconds_out.push((seg_end_dts - seg_start_dts) as f64 / vts as f64);
    }

    let target = segment_seconds_out.iter().fold(0f64, |m, s| m.max(*s)).ceil() as u64;
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init.mp4\"\n"
    );
    for (i, s) in segment_seconds_out.iter().enumerate() {
        playlist.push_str(&format!("#EXTINF:{s:.6},\nseg_{:05}.m4s\n", i + 1));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    let mut ordered = Vec::with_capacity(files.len() + 1);
    write_file(out_dir, "index.m3u8", playlist.as_bytes(), &mut ordered)?;
    ordered.extend(files);
    Ok(HlsPackage {
        files: ordered,
        segment_seconds: segment_seconds_out,
        duration_ms: crate::isobmff::units_to_ms(video.end(), video.timescale),
    })
}

/// Every file under `root/<rung>/` for the renditions upload (`<rung>/<file>`), each rung's
/// playlist first. Fails when a rung has no `index.m3u8`.
pub fn renditions_files(root: &Path, rungs: &[String]) -> Result<Vec<PackagedFile>> {
    let mut out = Vec::new();
    for rung in rungs {
        if !rung
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || rung.is_empty()
        {
            return Err(Error::invalid_input(format!("rung name {rung}")));
        }
        let dir = root.join(rung);
        let mut names: Vec<(String, u64)> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }
            names.push((name, entry.metadata()?.len()));
        }
        names.sort_by(|a, b| (a.0 != "index.m3u8", &a.0).cmp(&(b.0 != "index.m3u8", &b.0)));
        if names.first().is_none_or(|(n, _)| n != "index.m3u8") {
            return Err(Error::invalid_input(format!("{rung} has no index.m3u8")));
        }
        out.extend(names.into_iter().map(|(name, bytes)| PackagedFile {
            path: format!("{rung}/{name}"),
            bytes,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
