//! whisper.cpp inference via `whisper-rs`.

use std::path::Path;

use aun_common::{Error, Result};
use tokio_util::sync::CancellationToken;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{TranscribeOptions, Transcriber, Transcript, TranscriptSegment, TranscriptWord};

/// A loaded whisper model. Cheap to share; each call uses its own state.
pub struct WhisperTranscriber {
    context: WhisperContext,
}

impl std::fmt::Debug for WhisperTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperTranscriber").finish_non_exhaustive()
    }
}

fn platform(e: impl std::fmt::Display) -> Error {
    Error::Platform(format!("whisper: {e}"))
}

impl WhisperTranscriber {
    /// Loads a ggml model file.
    pub fn new(model_path: &Path) -> Result<Self> {
        let path = model_path
            .to_str()
            .ok_or_else(|| Error::InvalidInput("model path is not UTF-8".to_owned()))?;
        if !model_path.exists() {
            return Err(Error::NotFound(path.to_owned()));
        }
        let context =
            WhisperContext::new_with_params(path, WhisperContextParameters::default()).map_err(platform)?;
        Ok(Self { context })
    }
}

/// Whisper reports times in units of 10 ms.
fn centis_to_ms(t: i64) -> u64 {
    u64::try_from(t).unwrap_or(0) * 10
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(
        &self,
        pcm: &[f32],
        options: &TranscribeOptions,
        progress: &mut dyn FnMut(u32),
        cancel: &CancellationToken,
    ) -> Result<Transcript> {
        if pcm.is_empty() {
            return Err(Error::InvalidInput("no audio to transcribe".to_owned()));
        }
        let mut state = self.context.create_state().map_err(platform)?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(options.language.as_deref().unwrap_or("auto")));
        params.set_token_timestamps(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(threads) = options.threads {
            params.set_n_threads(i32::try_from(threads).unwrap_or(1).max(1));
        }
        // whisper-rs 0.16's `set_abort_callback_safe` stores `Box<Box<dyn FnMut>>` but its C
        // trampoline casts the pointer back to `*mut F`. With a plain closure that is a type
        // confusion (UB: it aborted the process in CI). Making `F` itself
        // `Box<dyn FnMut() -> bool>` makes the cast correct: the trampoline calls the outer box,
        // which forwards to ours. Keep this until whisper-rs fixes the trampoline.
        let abort = cancel.clone();
        let abort_cb: Box<dyn FnMut() -> bool> = Box::new(move || abort.is_cancelled());
        params.set_abort_callback_safe::<Box<dyn FnMut() -> bool>, Box<dyn FnMut() -> bool>>(abort_cb);
        let (sender, receiver) = std::sync::mpsc::channel::<u32>();
        params.set_progress_callback_safe(move |percent: i32| {
            let _ = sender.send(u32::try_from(percent).unwrap_or(0).min(100));
        });

        let result = state.full(params, pcm);
        while let Ok(percent) = receiver.try_recv() {
            progress(percent);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        result.map_err(platform)?;

        let language = whisper_rs::get_lang_str(state.full_lang_id_from_state())
            .unwrap_or_default()
            .to_owned();
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment.to_str_lossy().map_err(platform)?.trim().to_owned();
            let mut words: Vec<TranscriptWord> = Vec::new();
            for index in 0..segment.n_tokens() {
                let Some(token) = segment.get_token(index) else {
                    continue;
                };
                let piece = token.to_str_lossy().map_err(platform)?.into_owned();
                if piece.starts_with("[_") || piece.starts_with("<|") {
                    continue;
                }
                let data = token.token_data();
                let starts_word = piece.starts_with(' ') || words.is_empty();
                let (start, end) = (centis_to_ms(data.t0), centis_to_ms(data.t1));
                let confidence = f64::from(token.token_probability()).clamp(0.0, 1.0);
                match words.last_mut() {
                    Some(word) if !starts_word => {
                        word.text.push_str(&piece);
                        word.end_ms = end.max(word.end_ms);
                        word.confidence = word.confidence.min(confidence);
                    }
                    _ => words.push(TranscriptWord {
                        start_ms: start,
                        end_ms: end,
                        text: piece,
                        confidence,
                    }),
                }
            }
            for word in &mut words {
                word.text = word.text.trim().to_owned();
            }
            words.retain(|w| !w.text.is_empty());
            segments.push(TranscriptSegment {
                start_ms: centis_to_ms(segment.start_timestamp()),
                end_ms: centis_to_ms(segment.end_timestamp()),
                text,
                words,
            });
        }
        Ok(Transcript { language, segments })
    }
}
