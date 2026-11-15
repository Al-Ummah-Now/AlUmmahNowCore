//! HLS for desktop Studio's optional pre-encode (D11): the server's ladder, and packaging of one
//! progressive MP4 per rung into fMP4 HLS (`init.mp4`, `seg_00001.m4s`…, `index.m3u8`) laid out
//! like the server's own encode. The server verifies everything before using it.

mod ladder;
mod package;

pub use ladder::{
    FPS_CAP_ABOVE, GOP_SECONDS, HlsRung, MAX_FPS, SEGMENT_SECONDS, display_size, encode_fps, ladder,
};
pub use package::{HlsPackage, PackagedFile, package_mp4, renditions_files};
