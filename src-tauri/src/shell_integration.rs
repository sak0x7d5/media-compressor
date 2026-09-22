//! The "Shrink" entry in Explorer's right-click menu.
//!
//! Everything here writes under `HKEY_CURRENT_USER\Software\Classes`, which is
//! per-user and needs no administrator rights. It is registered per file
//! extension rather than under `*`, so the entry appears on videos and images
//! and stays off every text file and spreadsheet on the machine.
//!
//! This is opt-in and reversible: it is offered as a checkbox on the first-run
//! screen and can be toggled afterwards in Settings. [`register`] and
//! [`unregister`] are exact inverses. Uninstalling does the same cleanup from
//! `nsis/hooks.nsh`, because by then there may be no working copy of the app
//! left to run [`unregister`]; the tests below check the two agree.
//!
//! The entry is a cascade — "Shrink >" opening onto the preset sizes — built
//! from [`crate::shell_menu::MenuItem`]. Each extension only points at one
//! shared definition of that submenu, so adding a preset rewrites one key
//! rather than twenty.

use crate::shell_menu::MenuItem;
use thiserror::Error;

/// The key name we own. Distinct enough that we will never remove someone
/// else's entry when cleaning up.
const VERB: &str = "MediaCompressorCompress";

/// Deliberately not "Compress": Windows 11 puts its own "Compress to..." (which
/// makes a ZIP) in the same menu, and two neighbouring entries starting with
/// the same verb is a coin flip for the reader.
const MENU_TEXT: &str = "Shrink";

/// Where the submenu itself lives, relative to HKEY_CLASSES_ROOT. Every
/// extension references this one tree through `ExtendedSubCommandsKey`.
const MENU_KEY: &str = "MediaCompressor.ShrinkMenu";

/// The uninstaller's copy of the cleanup, compiled in only so the test below
/// can check it against [`EXTENSIONS`]. Never executed by the app.
#[cfg(test)]
const UNINSTALL_HOOK: &str = include_str!("../nsis/hooks.nsh");

/// Extensions that get the menu entry.
///
/// Deliberately excludes `.ts`: it is the MPEG transport stream extension, but
/// it is also TypeScript, and an extension association cannot tell the two
/// apart. Registering it would put "Compress for Discord" on every source file
/// on a developer machine, so transport streams have to be added by hand.
const EXTENSIONS: &[&str] = &[
    ".mp4", ".mov", ".mkv", ".webm", ".avi", ".m4v", ".wmv", ".flv", ".mpg", ".mpeg",
    ".gif", ".png", ".jpg", ".jpeg", ".webp", ".avif", ".bmp", ".tif", ".tiff",
];

/// Extensions an earlier build registered and this one no longer does.
///
/// Dropping one from [`EXTENSIONS`] stops it being written, but a machine that
/// enabled the menu under the old build still has the key, pointing at this
/// executable. Nothing else would ever delete it: [`register`] only writes the
/// current list, and a cleanup that iterates the current list walks straight
/// past it. So [`unregister`] and the uninstaller take both lists, and only
/// [`register`] and [`registered_count`] take the first.
const RETIRED_EXTENSIONS: &[&str] = &[".ts"];

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

    fn exe_path() -> Result<String, ShellError> {
        Ok(std::env::current_exe()
            .map_err(ShellError::ExePath)?
            .display()
            .to_string())
    }

    fn command_line(exe: &str, target_bytes: Option<u64>) -> String {
        match target_bytes {
            Some(bytes) => format!("\"{exe}\" --target {bytes} \"%1\""),
            None => format!("\"{exe}\" \"%1\""),
        }
    }

    fn menu_path() -> String {
        format!("Software\\Classes\\{MENU_KEY}")
    }

    /// Write one level of the cascade, recursing into submenus.
    fn write_items(
        hkcu: &RegKey,
        shell_path: &str,
        items: &[MenuItem],
        exe: &str,
    ) -> Result<(), ShellError> {
        for item in items {
            let path = format!("{shell_path}\\{}", item.key);
            let (key, _) = hkcu
                .create_subkey(&path)
                .map_err(|e| ShellError::Registry(format!("{path}: {e}")))?;

            key.set_value("MUIVerb", &item.label)
                .map_err(|e| ShellError::Registry(e.to_string()))?;

            if item.is_submenu() {
                // An empty SubCommands means "enumerate my own shell subkey",
                // which keeps the whole tree per-user. A semicolon-separated
                // list here would instead resolve against the machine-wide
                // CommandStore, which needs administrator rights to write.
                key.set_value("SubCommands", &"")
                    .map_err(|e| ShellError::Registry(e.to_string()))?;
                write_items(hkcu, &format!("{path}\\shell"), &item.children, exe)?;
            } else {
                // Lets Explorer pass several selected files as one invocation
                // rather than launching us once per file.
                key.set_value("MultiSelectModel", &"Player")
                    .map_err(|e| ShellError::Registry(e.to_string()))?;

                let (command, _) = key
                    .create_subkey("command")
                    .map_err(|e| ShellError::Registry(e.to_string()))?;
                command
                    .set_value("", &command_line(exe, item.target_bytes))
                    .map_err(|e| ShellError::Registry(e.to_string()))?;
            }
        }

        Ok(())
    }

    pub fn register(items: &[MenuItem]) -> Result<(), ShellError> {
        let exe = exe_path()?;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Both trees are rebuilt rather than merged into. A previous version of
        // this app wrote a `command` directly on the verb, and a verb carrying
        // both a command and a submenu is ambiguous; a shrunken preset list
        // would likewise leave orphaned entries behind.
        let _ = hkcu.delete_subkey_all(menu_path());
        write_items(&hkcu, &format!("{}\\shell", menu_path()), items, &exe)?;

        for extension in EXTENSIONS {
            let path = base_path(extension);
            let _ = hkcu.delete_subkey_all(&path);

            let (key, _) = hkcu
                .create_subkey(&path)
                .map_err(|e| ShellError::Registry(format!("{path}: {e}")))?;

            // MUIVerb rather than the default value: that is what Explorer
            // reads for the label of a verb that opens a submenu.
            key.set_value("MUIVerb", &MENU_TEXT)
                .map_err(|e| ShellError::Registry(e.to_string()))?;
            key.set_value("Icon", &exe)
                .map_err(|e| ShellError::Registry(e.to_string()))?;
            key.set_value("ExtendedSubCommandsKey", &MENU_KEY)
                .map_err(|e| ShellError::Registry(e.to_string()))?;
        }

        Ok(())
    }

    pub fn unregister() -> Result<(), ShellError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Absent is the desired end state, so "not found" is success.
        let _ = hkcu.delete_subkey_all(menu_path());

        for extension in EXTENSIONS.iter().chain(RETIRED_EXTENSIONS) {
            let path = base_path(extension);
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

    pub fn register(_items: &[MenuItem]) -> Result<(), ShellError> {
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

    /// `.ts` is a transport stream and also TypeScript. Claiming it puts the
    /// menu entry on every source file on a developer's machine, so it stays
    /// out no matter how video-shaped it looks.
    #[test]
    fn typescript_sources_are_not_claimed() {
        assert!(
            !EXTENSIONS.contains(&".ts"),
            ".ts is TypeScript as often as it is a transport stream"
        );
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

    /// The app removes these keys through [`unregister`], but an uninstall can
    /// happen without the app ever running again, so the NSIS uninstaller
    /// repeats the cleanup. Two hand-maintained lists drift, and the failure is
    /// silent — a key nobody ever deletes — so it is checked here instead.
    #[test]
    fn the_uninstaller_cleans_up_every_extension_we_register() {
        for extension in EXTENSIONS {
            let line = format!("!insertmacro RemoveShrinkVerb \"{extension}\"");
            assert!(
                UNINSTALL_HOOK.contains(&line),
                "nsis/hooks.nsh does not remove {extension}; add `{line}`"
            );
        }
    }

    /// Retired extensions are the other thing an uninstall has to clean up:
    /// the key an older build wrote is still there, still pointing at us.
    #[test]
    fn the_uninstaller_cleans_up_every_extension_we_used_to_register() {
        for extension in RETIRED_EXTENSIONS {
            let line = format!("!insertmacro RemoveShrinkVerb \"{extension}\"");
            assert!(
                UNINSTALL_HOOK.contains(&line),
                "nsis/hooks.nsh does not remove retired {extension}; add `{line}`"
            );
        }
    }

    /// The reverse direction: a line left behind for an extension we neither
    /// register nor used to is dead weight, and a sign the lists were edited
    /// apart.
    #[test]
    fn the_uninstaller_removes_nothing_we_do_not_register() {
        for line in UNINSTALL_HOOK.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("!insertmacro RemoveShrinkVerb ") else {
                continue;
            };
            let extension = rest.trim().trim_matches('"');
            assert!(
                EXTENSIONS.contains(&extension) || RETIRED_EXTENSIONS.contains(&extension),
                "nsis/hooks.nsh removes {extension}, which is in neither EXTENSIONS nor RETIRED_EXTENSIONS"
            );
        }
    }

    /// A retired extension is one we stopped writing, so it must not still be
    /// on the list we write.
    #[test]
    fn a_retired_extension_is_not_also_a_current_one() {
        for extension in RETIRED_EXTENSIONS {
            assert!(!EXTENSIONS.contains(extension), "{extension} is both current and retired");
        }
    }

    /// The uninstaller has to name the shared submenu key exactly, and it is
    /// spelled out there as a literal rather than shared with this constant.
    #[test]
    fn the_uninstaller_removes_the_shared_submenu_key() {
        assert!(
            UNINSTALL_HOOK.contains(MENU_KEY),
            "nsis/hooks.nsh must delete Software\\Classes\\{MENU_KEY}"
        );
    }

    /// The verb name is baked into the uninstaller's macro. Renaming it here
    /// without renaming it there would orphan every key on the next uninstall.
    #[test]
    fn the_uninstaller_uses_the_verb_name_we_register_under() {
        assert!(
            UNINSTALL_HOOK.contains(VERB),
            "nsis/hooks.nsh must delete the {VERB} verb"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_platform_without_the_registry_is_never_registered() {
        assert!(!is_supported());
        assert_eq!(registered_count(), 0);
        assert!(!is_registered());
    }
}
