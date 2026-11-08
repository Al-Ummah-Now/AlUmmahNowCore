//! Streaming audio decode (WAV, MP3, AAC, M4A, FLAC, OGG/Vorbis) built on Symphonia.

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Write};
use std::path::Path;

use aun_common::{Error, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio_util::sync::CancellationToken;

/// Sample rate and channel count of decoded PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
}

pub(crate) fn open_format(path: &Path) -> Result<Box<dyn FormatReader>> {
    let file = File::open(path).map_err(|e| open_error(path, e))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| match e {
            SymphoniaError::Unsupported(_) => {
                Error::Unsupported(format!("unrecognised audio format: {}", path.display()))
            }
            other => Error::InvalidInput(format!("cannot read {}: {other}", path.display())),
        })?;
    Ok(probed.format)
}

pub(crate) fn open_error(path: &Path, e: std::io::Error) -> Error {
    if e.kind() == ErrorKind::NotFound {
        Error::NotFound(path.display().to_string())
    } else {
        Error::Storage(format!("{}: {e}", path.display()))
    }
}

/// Decodes the first audio track, handing interleaved `f32` blocks to `on_block`.
///
/// Corrupt packets are skipped, as players do. `on_block` may return an error to stop early.
pub fn decode_audio(
    path: &Path,
    cancel: &CancellationToken,
    on_block: &mut dyn FnMut(AudioFormat, &[f32]) -> Result<()>,
) -> Result<AudioFormat> {
    if crate::refcontainer::is_reference_file(path)? {
        return crate::refcontainer::decode_audio(path, cancel, on_block);
    }
    let mut format = open_format(path)?;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| Error::InvalidInput(format!("no audio track in {}", path.display())))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Unsupported(format!("no decoder for {}: {e}", path.display())))?;

    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut detected: Option<AudioFormat> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(Error::InvalidInput(format!("demux failed: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(Error::InvalidInput(format!("decode failed: {e}"))),
        };
        let spec = *decoded.spec();
        let current = AudioFormat {
            sample_rate: spec.rate,
            channels: spec.channels.count() as u32,
        };
        match detected {
            None => detected = Some(current),
            Some(first) if first != current => {
                return Err(Error::Unsupported("audio format changes mid-stream".to_owned()));
            }
            Some(_) => {}
        }
        let frames = decoded.capacity() as u64;
        let buf = match &mut buffer {
            Some(b) if b.capacity() >= decoded.capacity() * spec.channels.count() => b,
            slot => slot.insert(SampleBuffer::<f32>::new(frames, spec)),
        };
        buf.copy_interleaved_ref(decoded);
        on_block(current, buf.samples())?;
    }
    detected.ok_or_else(|| Error::InvalidInput(format!("no audio decoded from {}", path.display())))
}

/// Writes interleaved samples as 16-bit PCM WAV. Used by the software backend and the test corpus.
pub fn write_wav_pcm16(path: &Path, format: AudioFormat, samples: &[f32]) -> Result<()> {
    if format.channels == 0 || format.sample_rate == 0 {
        return Err(Error::InvalidInput("empty audio format".to_owned()));
    }
    let data_len = samples
        .len()
        .checked_mul(2)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n <= u32::MAX - 36)
        .ok_or_else(|| Error::InvalidInput("audio too long for WAV".to_owned()))?;
    let mut out = BufWriter::new(File::create(path).map_err(|e| open_error(path, e))?);
    let block_align = format.channels * 2;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&(format.channels as u16).to_le_bytes())?;
    out.write_all(&format.sample_rate.to_le_bytes())?;
    out.write_all(&(format.sample_rate * block_align).to_le_bytes())?;
    out.write_all(&(block_align as u16).to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        out.write_all(&sample_to_i16(*s).to_le_bytes())?;
    }
    out.flush()?;
    Ok(())
}

pub(crate) fn sample_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let format = AudioFormat {
            sample_rate: 8000,
            channels: 2,
        };
        let samples: Vec<f32> = (0..800).map(|i| (i as f32 / 800.0) - 0.5).collect();
        write_wav_pcm16(&path, format, &samples).unwrap();
        let mut out = Vec::new();
        let got = decode_audio(&path, &CancellationToken::new(), &mut |_, b| {
            out.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, format);
        assert_eq!(out.len(), samples.len());
        assert!((out[10] - samples[10]).abs() < 0.001);
    }

    #[test]
    fn missing_file_is_not_found() {
        let err = decode_audio(
            Path::new("/nonexistent/a.wav"),
            &CancellationToken::new(),
            &mut |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn cancelled_before_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let format = AudioFormat {
            sample_rate: 8000,
            channels: 1,
        };
        write_wav_pcm16(&path, format, &[0.1; 8000]).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let err = decode_audio(&path, &token, &mut |_, _| Ok(())).unwrap_err();
        assert_eq!(err, Error::Cancelled);
    }

    #[test]
    fn garbage_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"this is not audio at all").unwrap();
        let err = decode_audio(&path, &CancellationToken::new(), &mut |_, _| Ok(())).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_) | Error::InvalidInput(_)));
    }
}
