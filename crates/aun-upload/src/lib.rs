//! Resumable TUS uploads for api-videos (Epic 11.4 / 19.2).
//!
//! - [`UploadEngine`]: persistent batch queue, bounded parallelism, shared bandwidth cap,
//!   Wi-Fi-only mode, checksums, restart recovery, proxy-expiry fallback to direct upload.
//! - [`TusClient`]: the TUS 1.0.0 calls on their own.
//! - [`watch`] (feature `watch`, desktop): folder watching that feeds the engine.

pub mod engine;
pub mod store;
pub mod throttle;
pub mod tus;
#[cfg(feature = "watch")]
pub mod watch;

pub use engine::{MAX_PARALLEL, UploadConfig, UploadEngine, UploadListener};
pub use store::{ManifestStore, UploadManifest, UploadRequest, UploadState, file_identity, sha256_file};
pub use throttle::Throttle;
pub use tus::{TUS_VERSION, TusClient, TusTarget, encode_metadata};

/// Guesses a video MIME type from a file extension (`None` when it isn't a video we accept).
pub fn video_mime_for(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mpg" | "mpeg" => "video/mpeg",
        "ts" | "mts" | "m2ts" => "video/mp2t",
        "3gp" => "video/3gpp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_guess() {
        assert_eq!(video_mime_for(std::path::Path::new("a/B.MP4")), Some("video/mp4"));
        assert_eq!(video_mime_for(std::path::Path::new("x.txt")), None);
        assert_eq!(video_mime_for(std::path::Path::new("noext")), None);
    }
}
