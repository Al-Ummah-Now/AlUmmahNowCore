//! Real inference. Runs in `.github/workflows/media.yml`, which supplies the model and clips:
//! `AUN_WHISPER_MODEL`, `AUN_CLIP_EN`, `AUN_CLIP_AR`.
#![cfg(feature = "whisper")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use aun_media::CancellationToken;
use aun_media::transcribe::{
    SubtitleOptions, TranscribeOptions, WhisperTranscriber, to_webvtt, transcribe_file,
};

fn transcribe(clip_var: &str, language: &str) -> aun_media::transcribe::Transcript {
    let model = std::env::var("AUN_WHISPER_MODEL").expect("AUN_WHISPER_MODEL");
    let clip = std::env::var(clip_var).expect(clip_var);
    let transcriber = WhisperTranscriber::new(Path::new(&model)).unwrap();
    let options = TranscribeOptions {
        language: Some(language.to_owned()),
        threads: Some(2),
    };
    let mut last_progress = 0;
    let transcript = transcribe_file(
        &transcriber,
        Path::new(&clip),
        &options,
        &mut |p| last_progress = p,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(!transcript.segments.is_empty());
    assert!(last_progress > 0);
    transcript
}

#[test]
#[ignore = "needs a whisper model; run by media.yml"]
fn transcribes_english() {
    let t = transcribe("AUN_CLIP_EN", "en");
    let text = t
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(text.contains("morning") || text.contains("lecture"), "{text}");
    let words: Vec<_> = t.segments.iter().flat_map(|s| s.words.iter()).collect();
    assert!(!words.is_empty());
    assert!(words.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    assert!(to_webvtt(&t, &SubtitleOptions::default()).starts_with("WEBVTT"));
}

#[test]
#[ignore = "needs a whisper model; run by media.yml"]
fn transcribes_arabic_script() {
    let t = transcribe("AUN_CLIP_AR", "ar");
    let text: String = t.segments.iter().map(|s| s.text.as_str()).collect();
    assert!(
        text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)),
        "{text}"
    );
}

#[test]
#[ignore = "needs a whisper model; run by media.yml"]
fn cancelled_inference_stops() {
    let model = std::env::var("AUN_WHISPER_MODEL").expect("AUN_WHISPER_MODEL");
    let transcriber = WhisperTranscriber::new(Path::new(&model)).unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let err = aun_media::transcribe::Transcriber::transcribe(
        &transcriber,
        &vec![0.0; 16_000],
        &TranscribeOptions::default(),
        &mut |_| {},
        &token,
    )
    .unwrap_err();
    assert_eq!(err, aun_common::Error::Cancelled);
}
