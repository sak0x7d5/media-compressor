//! The shape of the Explorer submenu, independent of how it gets written.
//!
//! Kept free of `winreg` and of Tauri so the interesting decisions — labels,
//! ordering, which entry carries which target — are checkable on any platform.
//! [`crate::shell_integration`] turns this into registry keys on Windows.

/// One entry in the cascade.
///
/// An item with children is a submenu and carries no command; an item without
/// them is clickable. `target_bytes` of `None` on a leaf means "open the app
/// and let the user pick", which is what the last entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Registry key name. Explorer orders submenu entries alphabetically by
    /// key, so these are numbered rather than named after their label.
    pub key: String,
    pub label: String,
    pub target_bytes: Option<u64>,
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn is_submenu(&self) -> bool {
        !self.children.is_empty()
    }
}

/// Sizes exactly as the app's own target picker renders them, so the menu and
/// the window never disagree about what "20 MB" means. Mirrors `formatBytes`
/// in `src/lib/format.ts`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    if bytes < 1000 * 1000 {
        return format!("{:.0} KB", bytes as f64 / 1000.0);
    }

    let mb = bytes as f64 / 1_000_000.0;
    if mb < 10.0 {
        format!("{mb:.2} MB")
    } else if mb < 1000.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.2} GB", mb / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_pickers_wording() {
        assert_eq!(format_bytes(20_000_000), "20.0 MB");
        assert_eq!(format_bytes(8_000_000), "8.00 MB");
        assert_eq!(format_bytes(500_000_000), "500.0 MB");
        assert_eq!(format_bytes(2_000_000_000), "2.00 GB");
        assert_eq!(format_bytes(25_000), "25 KB");
        assert_eq!(format_bytes(512), "512 B");
    }

    #[test]
    fn an_item_with_children_is_a_submenu() {
        let leaf = MenuItem {
            key: "00".into(),
            label: "x".into(),
            target_bytes: Some(1),
            children: Vec::new(),
        };
        assert!(!leaf.is_submenu());

        let parent = MenuItem { children: vec![leaf], ..leaf_template() };
        assert!(parent.is_submenu());
    }

    fn leaf_template() -> MenuItem {
        MenuItem { key: "01".into(), label: "y".into(), target_bytes: None, children: Vec::new() }
    }
}
