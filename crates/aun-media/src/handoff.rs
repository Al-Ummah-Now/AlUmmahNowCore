//! The record handed to `aun-upload` once an encode and transcript are ready (Epic 30.7).

use std::collections::BTreeMap;
use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::edl::ProjectChapter;
use crate::job::JobOutcome;
use crate::transcribe::{ChapterSuggestion, SubtitleOptions, Transcript, to_srt, to_webvtt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionFormat {
    Vtt,
    Srt,
}

/// A caption file written next to the upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionFile {
    pub locale: String,
    pub format: CaptionFormat,
    pub path: String,
}

/// A chapter, titled per locale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffChapter {
    pub start_ms: u64,
    pub title: BTreeMap<String, String>,
}

/// Everything `aun-upload` needs to publish a finished job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadHandoff {
    pub output_path: String,
    /// Locale → title.
    pub title: BTreeMap<String, String>,
    /// Locale → description.
    pub description: BTreeMap<String, String>,
    pub captions: Vec<CaptionFile>,
    pub chapters: Vec<HandoffChapter>,
    pub thumbnail_paths: Vec<String>,
}

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "heic", "pgm"];

fn is_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// Builds an [`UploadHandoff`] from a finished job.
#[derive(Debug, Clone)]
pub struct UploadHandoffBuilder {
    handoff: UploadHandoff,
}

impl UploadHandoffBuilder {
    /// Takes the job's first non-image output as the video and the images as thumbnails.
    pub fn from_outcome(outcome: &JobOutcome) -> Result<Self> {
        let output_path = outcome
            .outputs
            .iter()
            .find(|p| !is_image(p))
            .cloned()
            .ok_or_else(|| Error::InvalidInput(format!("job {} produced no media file", outcome.job_id)))?;
        Ok(Self {
            handoff: UploadHandoff {
                output_path,
                title: BTreeMap::new(),
                description: BTreeMap::new(),
                captions: Vec::new(),
                chapters: Vec::new(),
                thumbnail_paths: outcome.outputs.iter().filter(|p| is_image(p)).cloned().collect(),
            },
        })
    }

    pub fn title(mut self, locale: &str, text: &str) -> Self {
        self.handoff.title.insert(locale.to_owned(), text.to_owned());
        self
    }

    pub fn description(mut self, locale: &str, text: &str) -> Self {
        self.handoff
            .description
            .insert(locale.to_owned(), text.to_owned());
        self
    }

    /// Writes the transcript into `dir` as `captions.<locale>.vtt` and `.srt` and records both.
    /// The locale is the transcript's language.
    pub fn captions_from_transcript(
        mut self,
        transcript: &Transcript,
        dir: &Path,
        options: &SubtitleOptions,
    ) -> Result<Self> {
        let locale = &transcript.language;
        if locale.is_empty() || !locale.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(Error::InvalidInput(format!(
                "transcript has an unusable language {locale:?}"
            )));
        }
        std::fs::create_dir_all(dir)?;
        let write = |extension: &str, contents: String| -> Result<String> {
            let path = dir.join(format!("captions.{locale}.{extension}"));
            std::fs::write(&path, contents)?;
            Ok(path.to_string_lossy().into_owned())
        };
        let vtt = write("vtt", to_webvtt(transcript, options))?;
        let srt = write("srt", to_srt(transcript, options))?;
        self.handoff.captions.push(CaptionFile {
            locale: locale.clone(),
            format: CaptionFormat::Vtt,
            path: vtt,
        });
        self.handoff.captions.push(CaptionFile {
            locale: locale.clone(),
            format: CaptionFormat::Srt,
            path: srt,
        });
        Ok(self)
    }

    /// Adds transcript-derived chapter suggestions, titled in `locale`.
    pub fn suggested_chapters(mut self, chapters: &[ChapterSuggestion], locale: &str) -> Self {
        self.handoff
            .chapters
            .extend(chapters.iter().map(|c| HandoffChapter {
                start_ms: c.start_ms,
                title: BTreeMap::from([(locale.to_owned(), c.title.clone())]),
            }));
        self
    }

    /// Adds chapters taken from Studio project markers.
    pub fn project_chapters(mut self, chapters: &[ProjectChapter]) -> Self {
        self.handoff
            .chapters
            .extend(chapters.iter().map(|c| HandoffChapter {
                start_ms: c.start_ms,
                title: c.title.clone(),
            }));
        self
    }

    /// Finishes the record: requires a title, orders chapters and rejects duplicate start times.
    pub fn build(mut self) -> Result<UploadHandoff> {
        if self.handoff.title.values().all(|t| t.trim().is_empty()) {
            return Err(Error::InvalidInput(
                "an upload needs a title in at least one locale".to_owned(),
            ));
        }
        self.handoff.chapters.sort_by_key(|c| c.start_ms);
        if self
            .handoff
            .chapters
            .windows(2)
            .any(|w| w[0].start_ms == w[1].start_ms)
        {
            return Err(Error::InvalidInput(
                "two chapters start at the same time".to_owned(),
            ));
        }
        Ok(self.handoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::TranscriptSegment;

    fn outcome(outputs: &[&str]) -> JobOutcome {
        JobOutcome {
            job_id: "j".to_owned(),
            outputs: outputs.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn transcript(language: &str) -> Transcript {
        Transcript {
            language: language.to_owned(),
            segments: vec![TranscriptSegment {
                start_ms: 0,
                end_ms: 2000,
                text: "بسم الله".to_owned(),
                words: vec![],
            }],
        }
    }

    #[test]
    fn separates_video_from_thumbnails() {
        let handoff = UploadHandoffBuilder::from_outcome(&outcome(&["/t/a.JPG", "/o/v.mp4", "/t/b.png"]))
            .unwrap()
            .title("en", "Lecture")
            .build()
            .unwrap();
        assert_eq!(handoff.output_path, "/o/v.mp4");
        assert_eq!(handoff.thumbnail_paths, vec!["/t/a.JPG", "/t/b.png"]);
    }

    #[test]
    fn requires_a_media_file_and_a_title() {
        assert!(UploadHandoffBuilder::from_outcome(&outcome(&["/t/a.png"])).is_err());
        let builder = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"])).unwrap();
        assert!(builder.clone().build().is_err());
        assert!(builder.title("en", "  ").build().is_err());
    }

    #[test]
    fn writes_arabic_caption_files() {
        let dir = tempfile::tempdir().unwrap();
        let handoff = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"]))
            .unwrap()
            .title("ar", "محاضرة")
            .description("ar", "وصف")
            .captions_from_transcript(&transcript("ar"), dir.path(), &SubtitleOptions::default())
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(handoff.captions.len(), 2);
        assert_eq!(handoff.captions[0].format, CaptionFormat::Vtt);
        assert!(handoff.captions[0].path.ends_with("captions.ar.vtt"));
        let vtt = std::fs::read_to_string(&handoff.captions[0].path).unwrap();
        assert!(vtt.contains("بسم الله"));
        let srt = std::fs::read_to_string(&handoff.captions[1].path).unwrap();
        assert!(srt.starts_with("1\n"));
    }

    #[test]
    fn rejects_a_language_that_could_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let result = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"]))
            .unwrap()
            .captions_from_transcript(&transcript("../evil"), dir.path(), &SubtitleOptions::default());
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn merges_and_orders_chapters() {
        let suggested = [ChapterSuggestion {
            start_ms: 5000,
            title: "ثانيا".to_owned(),
        }];
        let project = [ProjectChapter {
            start_ms: 0,
            title: BTreeMap::from([("en".to_owned(), "Intro".to_owned())]),
        }];
        let handoff = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"]))
            .unwrap()
            .title("en", "t")
            .suggested_chapters(&suggested, "ar")
            .project_chapters(&project)
            .build()
            .unwrap();
        let starts: Vec<u64> = handoff.chapters.iter().map(|c| c.start_ms).collect();
        assert_eq!(starts, vec![0, 5000]);
        assert_eq!(handoff.chapters[1].title["ar"], "ثانيا");
    }

    #[test]
    fn duplicate_chapter_times_are_rejected() {
        let chapter = ChapterSuggestion {
            start_ms: 0,
            title: String::new(),
        };
        let result = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"]))
            .unwrap()
            .title("en", "t")
            .suggested_chapters(&[chapter.clone(), chapter], "en")
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn serialises_as_json() {
        let handoff = UploadHandoffBuilder::from_outcome(&outcome(&["v.mp4"]))
            .unwrap()
            .title("en", "t")
            .build()
            .unwrap();
        let json = serde_json::to_string(&handoff).unwrap();
        assert_eq!(serde_json::from_str::<UploadHandoff>(&json).unwrap(), handoff);
    }
}
