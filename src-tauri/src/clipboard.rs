//! Putting a finished file on the clipboard so it can be pasted into Discord.
//!
//! No file bytes are copied. `CF_HDROP` is a list of paths — exactly what
//! Explorer's Ctrl+C writes, which is why copying a 4 GB file is instant.
//! Chromium turns that format into a `DataTransfer.files` object on a paste
//! event, the same object a drag-and-drop produces, so an Electron app like
//! Discord cannot tell the two apart.
//!
//! **Verified against the Discord desktop client on 2026-08-21**: setting
//! `CF_HDROP` here and pressing Ctrl+V in a channel attaches the file. Worth
//! re-checking if Discord ever changes its paste handling, because the failure
//! mode is silent — the paste simply does nothing, or drops the path in as
//! text.

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("no files to copy")]
    Empty,

    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),

    #[error("{} is not an absolute path", .0.display())]
    NotAbsolute(PathBuf),

    #[error("could not open the clipboard; another application may be holding it")]
    Unavailable,

    #[error("could not write to the clipboard: {0}")]
    Write(String),

    #[error("copying files to the clipboard is only implemented on Windows")]
    Unsupported,
}

fn validate(paths: &[PathBuf]) -> Result<Vec<String>, ClipboardError> {
    if paths.is_empty() {
        return Err(ClipboardError::Empty);
    }

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        // Consumers of CF_HDROP resolve nothing — a relative path silently
        // becomes a paste of the wrong file, or of no file.
        if !path.is_absolute() {
            return Err(ClipboardError::NotAbsolute(path.clone()));
        }
        if !path.exists() {
            return Err(ClipboardError::Missing(path.clone()));
        }
        out.push(path.to_string_lossy().to_string());
    }
    Ok(out)
}

/// Place one or more files on the clipboard as a file reference.
#[cfg(windows)]
pub fn copy_files(paths: &[PathBuf]) -> Result<(), ClipboardError> {
    use clipboard_win::{options, raw, Clipboard};

    let rendered = validate(paths)?;

    // The clipboard is a global lock, so hold it across the write and no longer.
    let _clip = Clipboard::new_attempts(10).map_err(|_| ClipboardError::Unavailable)?;

    // `set_file_list` defaults to leaving existing formats in place. That
    // matters: a target offered both CF_TEXT and CF_HDROP may well take the
    // text, and paste the path as a message instead of attaching the file.
    raw::set_file_list_with(&rendered, options::DoClear)
        .map_err(|e| ClipboardError::Write(e.to_string()))
}

#[cfg(not(windows))]
pub fn copy_files(paths: &[PathBuf]) -> Result<(), ClipboardError> {
    validate(paths)?;
    Err(ClipboardError::Unsupported)
}

/// Open the containing folder with the file selected.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("explorer");
        // No space after the comma: `explorer /select, "x"` opens Documents
        // instead of selecting anything.
        command.arg(format!("/select,{}", path.display()));
        // explorer.exe returns a non-zero exit code even on success, so the
        // status is deliberately not checked.
        command.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").args(["-R"]).arg(path).spawn()?;
        Ok(())
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let parent = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(parent).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_list_is_rejected() {
        assert!(matches!(validate(&[]), Err(ClipboardError::Empty)));
    }

    #[test]
    fn relative_paths_are_rejected_before_anything_touches_the_clipboard() {
        let relative = PathBuf::from("some/relative/file.mp4");
        assert!(matches!(validate(&[relative]), Err(ClipboardError::NotAbsolute(_))));
    }

    #[test]
    fn missing_files_are_rejected() {
        let missing = std::env::temp_dir().join("definitely-not-here-9f2b.mp4");
        assert!(matches!(validate(&[missing]), Err(ClipboardError::Missing(_))));
    }

    #[test]
    fn a_real_absolute_file_validates() {
        let dir = std::env::temp_dir().join("media-compressor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("clip-{}.txt", std::process::id()));
        std::fs::write(&file, b"x").unwrap();

        let rendered = validate(&[file.clone()]).expect("a real absolute file should validate");
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("clip-"));

        let _ = std::fs::remove_file(&file);
    }
}
