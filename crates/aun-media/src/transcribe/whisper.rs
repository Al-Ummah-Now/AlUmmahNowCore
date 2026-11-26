//! whisper.cpp inference via `whisper-rs`.

use std::ffi::{CStr, c_char};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aun_common::{Error, Result};
use tokio_util::sync::CancellationToken;
use whisper_rs::whisper_rs_sys as sys;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{InferenceDevice, TranscribeOptions, Transcriber, Transcript, TranscriptSegment, TranscriptWord};

/// Built with a GPU backend (Metal or Vulkan).
const GPU_BUILD: bool = cfg!(any(feature = "whisper-metal", feature = "whisper-vulkan"));

/// A loaded whisper model. Cheap to share; each call uses its own state.
///
/// GPU builds load the model on the GPU when there is one. Loading it there or transcribing there
/// can still fail (too little video memory, a driver fault); the model is then loaded again on the
/// CPU, once, and used from then on.
pub struct WhisperTranscriber {
    context: Option<WhisperContext>,
    cpu: OnceLock<WhisperContext>,
    model_path: PathBuf,
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
            .ok_or_else(|| Error::invalid_input("model path is not UTF-8".to_owned()))?;
        if !model_path.exists() {
            return Err(Error::not_found(path.to_owned()));
        }
        let mut transcriber = Self {
            context: None,
            cpu: OnceLock::new(),
            model_path: model_path.to_owned(),
        };
        if GPU_BUILD && inference_device().gpu {
            match load(path, true) {
                Ok(context) => transcriber.context = Some(context),
                Err(e) => tracing::warn!("whisper: the GPU could not load the model, using the CPU: {e}"),
            }
        }
        if transcriber.context.is_none() {
            transcriber.cpu_context()?;
        }
        Ok(transcriber)
    }

    /// The model on the CPU, loaded on first use.
    fn cpu_context(&self) -> Result<&WhisperContext> {
        if let Some(context) = self.cpu.get() {
            return Ok(context);
        }
        let path = self
            .model_path
            .to_str()
            .ok_or_else(|| Error::invalid_input("model path is not UTF-8".to_owned()))?;
        // Two threads may both load it; the second copy is dropped
        let _ = self.cpu.set(load(path, false)?);
        self.cpu
            .get()
            .ok_or_else(|| Error::Platform("whisper: the CPU model was not kept".to_owned()))
    }
}

fn load(path: &str, gpu: bool) -> Result<WhisperContext> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(gpu);
    WhisperContext::new_with_params(path, params).map_err(platform)
}

fn device_text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: ggml returns NUL-terminated strings it owns for the life of the process
    #[allow(unsafe_code)]
    let text = unsafe { CStr::from_ptr(text) };
    text.to_string_lossy().trim().to_owned()
}

/// The device whisper uses: in GPU builds the first GPU ggml registered (the one whisper.cpp
/// picks), else the CPU. A Vulkan build without a Vulkan loader or driver registers no GPU.
#[must_use]
pub fn inference_device() -> InferenceDevice {
    let mut cpu = String::new();
    // SAFETY: ggml builds its device registry once, on first use, and never frees it; these calls
    // only read it, and every device pointer is checked before use.
    #[allow(unsafe_code)]
    unsafe {
        for index in 0..sys::ggml_backend_dev_count() {
            let device = sys::ggml_backend_dev_get(index);
            if device.is_null() {
                continue;
            }
            let kind = sys::ggml_backend_dev_type(device);
            let gpu = kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                || kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU;
            if GPU_BUILD && gpu {
                // ggml names devices "Metal", "Vulkan0", "Vulkan1", …
                let backend = device_text(sys::ggml_backend_dev_name(device))
                    .trim_end_matches(|c: char| c.is_ascii_digit())
                    .to_owned();
                return InferenceDevice {
                    gpu: true,
                    backend,
                    name: device_text(sys::ggml_backend_dev_description(device)),
                };
            }
            if kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_CPU && cpu.is_empty() {
                cpu = device_text(sys::ggml_backend_dev_description(device));
            }
        }
    }
    InferenceDevice {
        gpu: false,
        backend: "CPU".to_owned(),
        name: cpu,
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
            return Err(Error::invalid_input("no audio to transcribe".to_owned()));
        }
        // Progress never goes back, even when the CPU starts over after the GPU failed
        let mut shown = 0;
        let mut report = |percent: u32| {
            if percent > shown {
                shown = percent;
                progress(percent);
            }
        };
        if let Some(gpu) = &self.context {
            match run(gpu, pcm, options, &mut report, cancel) {
                Err(e) if !matches!(e, Error::Cancelled) => {
                    tracing::warn!("whisper: transcribing on the GPU failed, using the CPU: {e}");
                }
                done => return done,
            }
        }
        run(self.cpu_context()?, pcm, options, &mut report, cancel)
    }
}

fn run(
    context: &WhisperContext,
    pcm: &[f32],
    options: &TranscribeOptions,
    progress: &mut dyn FnMut(u32),
    cancel: &CancellationToken,
) -> Result<Transcript> {
    let mut state = context.create_state().map_err(platform)?;
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
