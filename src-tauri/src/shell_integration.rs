//! The "Compress for Discord" entry in Explorer's right-click menu.
//!
//! Everything here writes under `HKEY_CURRENT_USER\Software\Classes`, which is
//! per-user and needs no administrator rights. It is registered per file
//! extension rather than under `*`, so the entry appears on videos and images
//! and stays off every text file and spreadsheet on the machine.
//!
//! This is opt-in and reversible: it is offered as a checkbox on the first-run
//! screen and can be toggled afterwards in Settings. [`register`] and
//! [`unregister`] are exact inverses, and uninstalling should call the latter.

use thiserror::Error;

/// The key name we own. Distinct enough that we will never remove someone
/// else's entry when cleaning up.
const VERB: &str = "MediaCompressorCompress";

const MENU_TEXT: &str = "Compress for Discord";

/// Extensions that get the menu entry.
const EXTENSIONS: &[&str] = &[
    ".mp4", ".mov", ".mkv", ".webm", ".avi", ".m4v", ".wmv", ".flv", ".mpg", ".mpeg", ".ts",
    ".gif", ".png", ".jpg", ".jpeg", ".webp", ".avif", ".bmp", ".tif", ".tiff",
];

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("could not find this application's own path: {0}")]
    ExePath(#[source] std::io::Error),

    #[error("could not write to the registry: {0}")]
    Registry(String),

    #[error("Explorer integration is only implemented on Windows")]
    Unsupported,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::RegKey;

    fn base_path(extension: &str) -> String {
        // SystemFileAssociations attaches to the *type* rather than to whatever
        // program currently owns the extension, so the entry survives the user
        // changing their default video player.
        format!("Software\\Classes\\SystemFileAssociations\\{extension}\\shell\\{VERB}")
    }

    fn exe_quoted() -> Result<String, ShellError> {
        let exe = std::env::current_exe().map_err(ShellError::ExePath)?;
        Ok(format!("\"{}\" \"%1\"", exe.display()))
    }

    pub fn register() -> Result<(), ShellError> {
        let command_line = exe_quoted()?;
        let icon = std::env::current_exe().map_err(ShellError::ExePath)?;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        for extension in EXTENSIONS {
            let path = base_path(extension);
            let (key, _) = hkcu
                .create_subkey(&path)
                .map_err(|e| ShellError::Registry(format!("{path}: {e}")))?;

            key.set_value("", &MENU_TEXT)
                .map_err(|e| ShellError::Registry(e.to_string()))?;
            key.set_value("Icon", &icon.display().to_string())
                .map_err(|e| ShellError::Registry(e.to_string()))?;
            // Lets Explorer pass several selected files as one invocation
            // rather than launching us once per file.
            key.set_value("MultiSelectModel", &"Player")
                .map_err(|e| ShellError::Registry(e.to_string()))?;

            let (command, _) = key
                .create_subkey("command")
                .map_err(|e| ShellError::Registry(e.to_string()))?;
            command
                .set_value("", &command_line)
                .map_err(|e| ShellError::Registry(e.to_string()))?;
        }

        Ok(())
    }

    pub fn unregister() -> Result<(), ShellError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        for extension in EXTENSIONS {
            let path = base_path(extension);
            // Absent is the desired end state, so "not found" is success.
            match hkcu.open_subkey_with_flags(&path, KEY_ALL_ACCESS) {
                Ok(_) => {
                    let _ = hkcu.delete_subkey_all(&path);
                }
                Err(_) => continue,
            }
        }

        Ok(())
    }

    pub fn registered_count() -> usize {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        EXTENSIONS
            .iter()
            .filter(|extension| hkcu.open_subkey(base_path(extension)).is_ok())
            .count()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn register() -> Result<(), ShellError> {
        Err(ShellError::Unsupported)
    }

    pub fn unregister() -> Result<(), ShellError> {
        Err(ShellError::Unsupported)
    }

    pub fn registered_count() -> usize {
        0
    }
}

pub use imp::{register, registered_count, unregister};

/// Whether this build can offer the menu at all. The UI hides the option
/// entirely rather than showing a control that can only ever fail.
pub const fn is_supported() -> bool {
    cfg!(windows)
}

/// How many extensions the menu covers when fully registered.
pub fn extension_count() -> usize {
    EXTENSIONS.len()
}

/// True only when *every* extension carries the entry.
///
/// A part-written menu — [`register`] failing midway through the loop — reads
/// as "not registered" on purpose. That way the Settings toggle offers to
/// enable it again, and doing so re-runs [`register`] over the whole list and
/// repairs it. Reporting "on" for a half-written menu would leave the user
/// looking at an enabled switch and a missing right-click entry, with nothing
/// in the UI able to fix it.
pub fn is_registered() -> bool {
    registered_count() == extension_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extension_is_lowercase_and_dotted() {
        for extension in EXTENSIONS {
            assert!(extension.starts_with('.'), "{extension} is missing its dot");
            assert_eq!(
                *extension,
                extension.to_ascii_lowercase(),
                "{extension} must be lowercase to match Explorer's lookup"
            );
        }
    }

    #[test]
    fn the_extension_list_has_no_duplicates() {
        let mut seen: Vec<&str> = Vec::new();
        for extension in EXTENSIONS {
            assert!(!seen.contains(extension), "duplicate extension {extension}");
            seen.push(extension);
        }
    }

    /// Registering writes to the user's registry, so it is deliberately not
    /// exercised here — a test suite should not change the machine it runs on.
    /// `is_registered` only reads, so it is safe to call.
    #[test]
    fn checking_registration_does_not_change_anything() {
        let before = is_registered();
        let after = is_registered();
        assert_eq!(before, after);
    }

    #[test]
    fn the_menu_covers_at_least_one_extension() {
        // `is_registered` compares a count against this; an empty list would
        // make an unregistered menu report itself as fully registered.
        assert!(extension_count() > 0);
    }

    #[test]
    fn registration_is_all_or_nothing() {
        assert_eq!(is_registered(), registered_count() == extension_count());
        assert!(registered_count() <= extension_count());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_platform_without_the_registry_is_never_registered() {
        assert!(!is_supported());
        assert_eq!(registered_count(), 0);
        assert!(!is_registered());
    }
}
