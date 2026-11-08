//! The software backend's reference container: `AUNV1\n`, one JSON header line, then the payload of
//! each track. Audio payloads are 16-bit little-endian PCM. It lets the runner and the conformance
//! suite be exercised end to end without a real encoder, and is never shipped to viewers.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audio::{AudioFormat, open_error};
use crate::probe::{TrackInfo, TrackKind};

const MAGIC: &[u8] = b"AUNV1\n";
const MAX_HEADER: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RefTrack {
    pub kind: TrackKind,
    pub codec: String,
    pub duration_ms: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub payload_len: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RefHeader {
    pub tracks: Vec<RefTrack>,
}

impl RefHeader {
    pub fn track_infos(&self) -> Vec<TrackInfo> {
        self.tracks
            .iter()
            .enumerate()
            .map(|(index, t)| TrackInfo {
                index: index as u32,
                kind: t.kind,
                codec: t.codec.clone(),
                duration_ms: Some(t.duration_ms),
                width: t.width,
                height: t.height,
                fps: t.fps,
                sample_rate: t.sample_rate,
                channels: t.channels,
                language: None,
            })
            .collect()
    }
}

pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

pub(crate) fn is_reference_file(path: &Path) -> Result<bool> {
    let mut file = File::open(path).map_err(|e| open_error(path, e))?;
    let mut head = [0u8; MAGIC.len()];
    match file.read_exact(&mut head) {
        Ok(()) => Ok(sniff(&head)),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Writes tracks to `path` via a temporary sibling, so a failed job never leaves a partial output.
pub(crate) fn write(path: &Path, tracks: &[(RefTrack, Vec<u8>)]) -> Result<()> {
    let header = RefHeader {
        tracks: tracks.iter().map(|(t, _)| t.clone()).collect(),
    };
    let mut temp = path.as_os_str().to_owned();
    temp.push(".partial");
    let temp = std::path::PathBuf::from(temp);
    let result = (|| -> Result<()> {
        let mut out = File::create(&temp).map_err(|e| open_error(&temp, e))?;
        out.write_all(MAGIC)?;
        out.write_all(serde_json::to_string(&header)?.as_bytes())?;
        out.write_all(b"\n")?;
        for (_, payload) in tracks {
            out.write_all(payload)?;
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub(crate) fn read_header(path: &Path) -> Result<(RefHeader, u64)> {
    let file = File::open(path).map_err(|e| open_error(path, e))?;
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if !sniff(&magic) {
        return Err(Error::InvalidInput("not a reference container".to_owned()));
    }
    let mut line = Vec::new();
    reader.by_ref().take(MAX_HEADER).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(Error::InvalidInput(
            "reference container header is truncated".to_owned(),
        ));
    }
    let header: RefHeader = serde_json::from_slice(&line)
        .map_err(|e| Error::InvalidInput(format!("reference container header: {e}")))?;
    Ok((header, MAGIC.len() as u64 + line.len() as u64))
}

/// Reads the whole payload of track `index`.
pub(crate) fn read_payload(path: &Path, index: usize) -> Result<(RefTrack, Vec<u8>)> {
    let (header, data_offset) = read_header(path)?;
    let track = header
        .tracks
        .get(index)
        .cloned()
        .ok_or_else(|| Error::InvalidInput(format!("no track {index}")))?;
    let offset = header.tracks[..index]
        .iter()
        .fold(data_offset, |o, t| o.saturating_add(t.payload_len));
    let mut file = File::open(path).map_err(|e| open_error(path, e))?;
    let available = file.metadata()?.len().saturating_sub(offset);
    if track.payload_len > available {
        return Err(Error::InvalidInput(
            "reference container payload is truncated".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut payload = vec![0u8; track.payload_len as usize];
    file.read_exact(&mut payload)?;
    Ok((track, payload))
}

/// Decodes the first audio track as interleaved `f32`.
pub(crate) fn decode_audio(
    path: &Path,
    cancel: &CancellationToken,
    on_block: &mut dyn FnMut(AudioFormat, &[f32]) -> Result<()>,
) -> Result<AudioFormat> {
    let (header, _) = read_header(path)?;
    let index = header
        .tracks
        .iter()
        .position(|t| t.kind == TrackKind::Audio)
        .ok_or_else(|| Error::InvalidInput(format!("no audio track in {}", path.display())))?;
    let (track, payload) = read_payload(path, index)?;
    let format = AudioFormat {
        sample_rate: track.sample_rate.unwrap_or(0),
        channels: track.channels.unwrap_or(0),
    };
    if format.sample_rate == 0 || format.channels == 0 {
        return Err(Error::InvalidInput("audio track has no format".to_owned()));
    }
    let samples = pcm_to_f32(&payload);
    let block = (format.sample_rate as usize / 10).max(1) * format.channels as usize;
    for chunk in samples.chunks(block) {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        on_block(format, chunk)?;
    }
    Ok(format)
}

pub(crate) fn pcm_to_f32(payload: &[u8]) -> Vec<f32> {
    payload
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f32::from(i16::from_le_bytes(*b)) / f32::from(i16::MAX))
        .collect()
}
