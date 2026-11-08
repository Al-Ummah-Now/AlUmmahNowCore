//! Container and codec probing without ffmpeg.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error as SymphoniaError;

use crate::audio::{open_error, open_format};
use crate::isobmff::{self, Movie, Trak};
use crate::refcontainer;

/// The container family of a probed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerFormat {
    Mp4,
    Mov,
    M4a,
    Wav,
    Mp3,
    Aac,
    Flac,
    Ogg,
    /// The software backend's reference container (`crate::job::SoftwareBackend`).
    Reference,
    /// Unknown to the sniffer but readable by the audio demuxer.
    Other,
}

/// What a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

/// One track of a media file. Fields that do not apply to the kind are `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub index: u32,
    pub kind: TrackKind,
    /// Lower-case codec name: `h264`, `hevc`, `aac`, `opus`, `mp3`, `flac`, `pcm_s16le`, …
    pub codec: String,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// ISO 639-2 language tag when the container records one.
    pub language: Option<String>,
}

/// The result of [`probe_file`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: ContainerFormat,
    pub duration_ms: Option<u64>,
    pub size_bytes: u64,
    /// Whole-file bitrate in kilobits per second, when the duration is known.
    pub bitrate_kbps: Option<u64>,
    pub tracks: Vec<TrackInfo>,
}

impl MediaInfo {
    /// The first track of `kind`.
    pub fn first_track(&self, kind: TrackKind) -> Option<&TrackInfo> {
        self.tracks.iter().find(|t| t.kind == kind)
    }
}

/// Reads container metadata: tracks, codecs, duration, resolution and frame rate.
pub fn probe_file(path: &Path) -> Result<MediaInfo> {
    let mut file = File::open(path).map_err(|e| open_error(path, e))?;
    let size_bytes = file.metadata()?.len();
    let mut head = [0u8; 16];
    let head_len = read_up_to(&mut file, &mut head)?;
    drop(file);
    let head = &head[..head_len];

    let (container, duration_ms, tracks) = if refcontainer::sniff(head) {
        let header = refcontainer::read_header(path)?.0;
        (
            ContainerFormat::Reference,
            header.tracks.iter().map(|t| t.duration_ms).max(),
            header.track_infos(),
        )
    } else if isobmff::sniff(head) {
        let movie = isobmff::parse_file(path)?;
        let tracks = movie.tracks.iter().enumerate().map(iso_track).collect::<Vec<_>>();
        (iso_container(&movie), iso_duration(&movie), tracks)
    } else {
        let (duration, tracks) = probe_with_symphonia(path)?;
        (sniff_audio_container(head), duration, tracks)
    };
    let bitrate_kbps = duration_ms
        .filter(|d| *d > 0)
        .map(|d| size_bytes.saturating_mul(8) / d);
    Ok(MediaInfo {
        container,
        duration_ms,
        size_bytes,
        bitrate_kbps,
        tracks,
    })
}

/// Keyframe times (ms) of the first video track of an MP4/MOV, for stream-copy decisions.
/// Returns [`Error::Unsupported`] for other containers and `InvalidInput` when there is no video.
pub fn probe_keyframes(path: &Path) -> Result<Vec<u64>> {
    let mut file = File::open(path).map_err(|e| open_error(path, e))?;
    let mut head = [0u8; 16];
    let n = read_up_to(&mut file, &mut head)?;
    drop(file);
    if !isobmff::sniff(&head[..n]) {
        return Err(Error::Unsupported(
            "keyframe index needs an MP4 or MOV file".to_owned(),
        ));
    }
    let movie = isobmff::parse_file(path)?;
    movie
        .tracks
        .iter()
        .find(|t| t.handler == "vide")
        .map(Trak::keyframes_ms)
        .ok_or_else(|| Error::InvalidInput(format!("no video track in {}", path.display())))
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

fn iso_container(movie: &Movie) -> ContainerFormat {
    let has_brand = |b: &str| movie.brand == b || movie.compatible.iter().any(|c| c == b);
    if movie.brand == "qt  " {
        ContainerFormat::Mov
    } else if has_brand("M4A ") || has_brand("M4B ") {
        ContainerFormat::M4a
    } else {
        ContainerFormat::Mp4
    }
}

fn iso_duration(movie: &Movie) -> Option<u64> {
    if movie.duration > 0 && movie.timescale > 0 {
        return Some(isobmff::units_to_ms(movie.duration, movie.timescale));
    }
    movie.tracks.iter().filter_map(Trak::duration_ms).max()
}

fn iso_track((index, t): (usize, &Trak)) -> TrackInfo {
    let kind = match t.handler.as_str() {
        "vide" => TrackKind::Video,
        "soun" => TrackKind::Audio,
        "sbtl" | "text" | "subt" | "clcp" => TrackKind::Subtitle,
        _ => TrackKind::Other,
    };
    let (mut width, mut height, mut fps, mut sample_rate, mut channels) = (None, None, None, None, None);
    match kind {
        TrackKind::Video => {
            width = (t.width > 0).then_some(t.width);
            height = (t.height > 0).then_some(t.height);
            fps = t.fps();
        }
        TrackKind::Audio => {
            sample_rate = (t.sample_rate > 0).then_some(t.sample_rate);
            channels = (t.channels > 0).then_some(t.channels);
        }
        _ => {}
    }
    TrackInfo {
        index: index as u32,
        kind,
        codec: iso_codec_name(&t.sample_entry),
        duration_ms: t.duration_ms(),
        width,
        height,
        fps,
        sample_rate,
        channels,
        language: t.language.clone(),
    }
}

fn iso_codec_name(entry: &str) -> String {
    match entry {
        "avc1" | "avc3" => "h264",
        "hev1" | "hvc1" => "hevc",
        "av01" => "av1",
        "vp09" => "vp9",
        "mp4v" => "mpeg4",
        "mp4a" => "aac",
        "Opus" => "opus",
        "fLaC" => "flac",
        "alac" => "alac",
        "ac-3" => "ac3",
        "ec-3" => "eac3",
        "tx3g" => "tx3g",
        "wvtt" => "webvtt",
        other => return other.trim().to_lowercase(),
    }
    .to_owned()
}

fn sniff_audio_container(head: &[u8]) -> ContainerFormat {
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        ContainerFormat::Wav
    } else if head.starts_with(b"fLaC") {
        ContainerFormat::Flac
    } else if head.starts_with(b"OggS") {
        ContainerFormat::Ogg
    } else if head.starts_with(b"ID3") {
        ContainerFormat::Mp3
    } else if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 {
        // MPEG sync: layer bits are zero for ADTS AAC and non-zero for MP3.
        if head[1] & 0x06 == 0 {
            ContainerFormat::Aac
        } else {
            ContainerFormat::Mp3
        }
    } else {
        ContainerFormat::Other
    }
}

fn probe_with_symphonia(path: &Path) -> Result<(Option<u64>, Vec<TrackInfo>)> {
    let mut format = open_format(path)?;
    let mut tracks = Vec::new();
    let mut needs_scan = None;
    for (index, track) in format.tracks().iter().enumerate() {
        let params = &track.codec_params;
        if params.codec == CODEC_TYPE_NULL {
            continue;
        }
        let duration_ms = match (params.n_frames, params.time_base) {
            (Some(n), Some(tb)) => {
                let t = tb.calc_time(n);
                Some(t.seconds * 1000 + (t.frac * 1000.0) as u64)
            }
            (Some(n), None) => params
                .sample_rate
                .filter(|r| *r > 0)
                .map(|r| n * 1000 / u64::from(r)),
            _ => None,
        };
        if duration_ms.is_none() && needs_scan.is_none() {
            needs_scan = Some((track.id, params.time_base, params.sample_rate));
        }
        tracks.push(TrackInfo {
            index: index as u32,
            kind: TrackKind::Audio,
            codec: symphonia_codec_name(params.codec),
            duration_ms,
            width: None,
            height: None,
            fps: None,
            sample_rate: params.sample_rate,
            channels: params.channels.map(|c| c.count() as u32),
            language: track.language.clone(),
        });
    }
    if tracks.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no audio track in {}",
            path.display()
        )));
    }
    if let Some((id, time_base, rate)) = needs_scan {
        let mut units = 0u64;
        loop {
            match format.next_packet() {
                Ok(p) if p.track_id() == id => units = units.saturating_add(p.dur),
                Ok(_) => {}
                Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(Error::InvalidInput(format!("demux failed: {e}"))),
            }
        }
        let ms = match (time_base, rate) {
            (Some(tb), _) => {
                let t = tb.calc_time(units);
                Some(t.seconds * 1000 + (t.frac * 1000.0) as u64)
            }
            (None, Some(r)) if r > 0 => Some(units * 1000 / u64::from(r)),
            _ => None,
        };
        if let Some(track) = tracks.iter_mut().find(|t| t.duration_ms.is_none()) {
            track.duration_ms = ms;
        }
    }
    let duration = tracks.iter().filter_map(|t| t.duration_ms).max();
    Ok((duration, tracks))
}

fn symphonia_codec_name(codec: symphonia::core::codecs::CodecType) -> String {
    if codec == CODEC_TYPE_OPUS {
        return "opus".to_owned();
    }
    symphonia::default::get_codecs()
        .get_codec(codec)
        .map_or_else(|| format!("{codec}"), |d| d.short_name.to_lowercase())
}
