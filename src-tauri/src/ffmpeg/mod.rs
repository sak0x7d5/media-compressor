//! The FFmpeg layer: locating the binaries, asking them about a file, and
//! driving them to produce one.
//!
//! FFmpeg runs as a child process rather than as linked libraries. That keeps
//! the build simple on Windows, lets a malformed file kill a process instead of
//! the app, and keeps a GPL-licensed binary at arm's length from our code.

pub mod acquire;
pub mod encode;
pub mod probe;
pub mod sample;
pub mod system;
pub mod tools;

pub use acquire::{AcquireError, AcquireProgress};
pub use encode::{CancelToken, EncodeError, EncodeJob, EncodeProgress, Speed};
pub use probe::{probe, ProbeError};
pub use tools::{FfmpegTools, ToolsError, ToolsSource};

/// The FFmpeg this launch will use, or `None` if one has to be downloaded.
///
/// Order matters and is not arbitrary. The app's own copy wins because it is
/// the one we verified and the one an existing install is already using —
/// switching someone to a different encoder behind their back on an ordinary
/// launch would be a surprise. Only when there is no copy of ours does a build
/// already on the machine get considered, and then only on its merits.
pub fn resolve(cache_dir: &std::path::Path) -> Option<FfmpegTools> {
    FfmpegTools::locate(cache_dir).ok().or_else(|| system::adopt(cache_dir))
}

/// Stop Windows opening a console window for a child process.
///
/// Without this, every probe and every encode flashes a black box over the
/// app. A queue of twenty files would strobe.
#[cfg(windows)]
pub(crate) fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_console(_command: &mut std::process::Command) {}
