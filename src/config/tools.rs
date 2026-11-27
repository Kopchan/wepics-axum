//! Detection of optional runtime tools.
//!
//! FFmpeg and FFprobe are external programs, and libvips is a native library:
//! any of them may be missing on a user's machine. The server still starts and
//! degrades explicitly instead of failing every request, so the capabilities
//! are detected once at startup and reported.

use std::path::Path;
use std::process::{Command, Stdio};

/// Which optional pieces of the media pipeline are usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// libvips loaded and initialized; image previews can be rendered.
    pub vips: bool,
    /// `ffmpeg` runs; frames can be extracted and videos transcoded.
    pub ffmpeg: bool,
    /// `ffprobe` runs; video/animation metadata can be read.
    pub ffprobe: bool,
}

impl Capabilities {
    /// Optimistic defaults for tests and environments where probing at
    /// construction time is undesirable.
    pub const fn assume_available() -> Self {
        Self {
            vips: true,
            ffmpeg: true,
            ffprobe: true,
        }
    }

    /// Probes every tool once. Probing is cheap: libvips initialization is
    /// cached process-wide and the binaries only print their version.
    pub fn detect(ffmpeg: &Path, ffprobe: &Path) -> Self {
        Self {
            vips: crate::config::vips::ensure().is_ok(),
            ffmpeg: binary_available(ffmpeg),
            ffprobe: binary_available(ffprobe),
        }
    }

    /// Prints the individual capabilities in one line, including the tools
    /// that are present when libvips itself is missing.
    pub fn report(&self) {
        let mark = |available: bool| if available { "ok" } else { "missing" };
        println!(
            "Tools:    libvips {}, ffmpeg {}, ffprobe {}",
            mark(self.vips),
            mark(self.ffmpeg),
            mark(self.ffprobe)
        );

        if !self.vips {
            eprintln!(
                "Warning:  libvips is missing; image/vector previews redirect to the original \
                 and video/animation previews require an exported frame."
            );
        }
        if !self.ffmpeg {
            eprintln!(
                "Warning:  ffmpeg is missing; video frames cannot be extracted and transcodes \
                 redirect to the original."
            );
        }
        if !self.ffprobe {
            eprintln!("Warning:  ffprobe is missing; video files are not indexed.");
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::assume_available()
    }
}

/// Runs `<binary> -version`. Any successful exit means the tool works, even if
/// the wrapper prints to stderr.
fn binary_available(binary: &Path) -> bool {
    Command::new(binary)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
