//! The changelog, parsed.
//!
//! `CHANGELOG.md` is compiled into the binary rather than shipped as a resource
//! or fetched. "What changed in the version I just installed" is a question the
//! app should be able to answer on a plane, and a file that cannot go missing
//! cannot fail to load.
//!
//! The parser understands a deliberately small slice of Markdown — `##` for a
//! release, `###` for a group, `-` for an item, indentation for a wrapped line.
//! Anything else in the file is prose for whoever is editing it and is ignored
//! here. Keeping the grammar this small is what lets the whole thing stay pure
//! and testable.

use serde::Serialize;

/// The shipped changelog. A test asserts its newest entry is this build, so a
/// release cannot go out advertising notes that describe something else.
const BUILT_IN: &str = include_str!("../../CHANGELOG.md");

/// A group of related items — "Added", "Fixed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub heading: String,
    pub items: Vec<String>,
}

/// One released version's entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub sections: Vec<Section>,
}

impl Release {
    /// Every item, flattened. Used where the grouping would be noise.
    pub fn items(&self) -> impl Iterator<Item = &str> + '_ {
        self.sections.iter().flat_map(|section| section.items.iter().map(String::as_str))
    }
}

/// Items that appear before any `###` heading still need somewhere to live.
const DEFAULT_HEADING: &str = "Changes";

/// The compiled-in changelog, newest first.
pub fn releases() -> Vec<Release> {
    parse(BUILT_IN)
}

/// Parse a Keep a Changelog document into releases, newest first.
///
/// Entries that don't name a parseable version — `## [Unreleased]` being the
/// one that matters — are dropped along with everything under them. An entry
/// nobody can install is not a change anyone can be told about.
pub fn parse(markdown: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    let mut current: Option<Release> = None;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix("## ") {
            if let Some(finished) = current.take() {
                releases.push(finished);
            }
            current = parse_heading(heading).map(|(version, date)| Release {
                version,
                date,
                sections: Vec::new(),
            });
            continue;
        }

        // Anything outside a release entry — the file's own preamble, or the
        // body of an entry we chose to drop — is not ours to keep.
        let Some(release) = current.as_mut() else {
            continue;
        };

        if let Some(heading) = trimmed.strip_prefix("### ") {
            release.sections.push(Section {
                heading: heading.trim().to_string(),
                items: Vec::new(),
            });
        } else if let Some(item) =
            trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "))
        {
            section_for(release).items.push(item.trim().to_string());
        } else if line.starts_with([' ', '\t']) {
            // An indented line continues the bullet above it. The file is hard
            // wrapped at 80 columns, so without this every second item would
            // arrive cut in half.
            if let Some(last) = release.sections.last_mut().and_then(|s| s.items.last_mut()) {
                last.push(' ');
                last.push_str(trimmed);
            }
        }
    }

    if let Some(finished) = current.take() {
        releases.push(finished);
    }

    releases
}

fn section_for(release: &mut Release) -> &mut Section {
    if release.sections.is_empty() {
        release.sections.push(Section {
            heading: DEFAULT_HEADING.to_string(),
            items: Vec::new(),
        });
    }
    release.sections.last_mut().expect("just ensured one exists")
}

/// `[0.2.0] — 2026-08-21` → `("0.2.0", Some("2026-08-21"))`.
///
/// The brackets and the dash are both optional, because the one thing worse
/// than a strict format is a release entry silently vanishing from the app
/// because someone typed an en dash.
fn parse_heading(heading: &str) -> Option<(String, Option<String>)> {
    let heading = heading.trim();

    let (version, rest) = match heading.strip_prefix('[') {
        Some(after) => {
            let end = after.find(']')?;
            (&after[..end], &after[end + 1..])
        }
        None => match heading.find(char::is_whitespace) {
            Some(end) => (&heading[..end], &heading[end..]),
            None => (heading, ""),
        },
    };

    // "Unreleased" lands here and fails, which is exactly what should happen.
    parse_version(version)?;

    let date = rest
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '-' | '–' | '—' | '(' | ')' | ':')
        })
        .to_string();

    Some((version.trim().to_string(), (!date.is_empty()).then_some(date)))
}

/// `1.2.3` → `(1, 2, 3)`. Pre-release and build suffixes are ignored: this is
/// used for ordering, and `0.3.0-beta.1` sorts with the 0.3.0 line.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let core = text
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;

    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    // A two-part version is a version. Missing parts are zero, not a failure.
    let minor = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;

    if parts.next().is_some() {
        return None;
    }

    Some((major, minor, patch))
}

/// Is `candidate` a later version than `than`?
///
/// Compared as numbers, not as text. `"0.10.0" > "0.9.0"` is false as a string
/// comparison, and an app that believed that would stop offering updates at
/// exactly the point it started having a history.
pub fn is_newer(candidate: &str, than: &str) -> bool {
    match (parse_version(candidate), parse_version(than)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The entries strictly newer than `version`, newest first.
pub fn since(releases: &[Release], version: &str) -> Vec<Release> {
    releases
        .iter()
        .filter(|release| is_newer(&release.version, version))
        .cloned()
        .collect()
}

/// The entry for one specific version, if the file has one.
pub fn find(releases: &[Release], version: &str) -> Option<Release> {
    let wanted = parse_version(version)?;
    releases
        .iter()
        .find(|release| parse_version(&release.version) == Some(wanted))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_changelog_describes_this_exact_build() {
        let releases = releases();
        let newest = releases.first().expect("the changelog must have an entry");

        assert_eq!(
            parse_version(&newest.version),
            parse_version(env!("CARGO_PKG_VERSION")),
            "the newest changelog entry is {} but this build is {} — the update \
             prompt would describe the wrong release",
            newest.version,
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn entries_are_ordered_newest_first() {
        let releases = releases();
        for pair in releases.windows(2) {
            assert!(
                is_newer(&pair[0].version, &pair[1].version),
                "{} is listed above {} but is not newer",
                pair[0].version,
                pair[1].version
            );
        }
    }

    #[test]
    fn every_bundled_entry_says_something() {
        for release in releases() {
            assert!(release.date.is_some(), "{} has no date", release.version);
            assert!(
                release.items().next().is_some(),
                "{} has no items — an empty entry is worse than no entry",
                release.version
            );
        }
    }

    const SAMPLE: &str = "\
# Changelog

Some prose about how to edit this file.

## [Unreleased]

### Added
- Something not shipped yet.

## [0.2.0] — 2026-08-21

### Added
- A thing that is long enough to wrap
  onto a second line.
- A second thing.

### Fixed
- A bug.

## 0.1.0

- An item with no heading above it.
";

    #[test]
    fn a_release_parses_into_its_groups() {
        let releases = parse(SAMPLE);
        let latest = &releases[0];

        assert_eq!(latest.version, "0.2.0");
        assert_eq!(latest.date.as_deref(), Some("2026-08-21"));
        assert_eq!(latest.sections.len(), 2);
        assert_eq!(latest.sections[0].heading, "Added");
        assert_eq!(latest.sections[1].items, vec!["A bug."]);
    }

    #[test]
    fn wrapped_items_are_joined_back_together() {
        let releases = parse(SAMPLE);
        assert_eq!(
            releases[0].sections[0].items[0],
            "A thing that is long enough to wrap onto a second line."
        );
    }

    #[test]
    fn an_unreleased_entry_is_never_offered() {
        let releases = parse(SAMPLE);

        assert!(
            releases.iter().all(|release| release.version != "Unreleased"),
            "Unreleased is not a version anyone can install"
        );
        assert!(
            releases.iter().all(|r| r.items().all(|item| !item.contains("not shipped"))),
            "items under Unreleased must not leak into the entry above or below it"
        );
    }

    #[test]
    fn items_before_any_heading_still_land_somewhere() {
        let releases = parse(SAMPLE);
        let oldest = releases.last().unwrap();

        assert_eq!(oldest.version, "0.1.0");
        assert_eq!(oldest.date, None);
        assert_eq!(oldest.sections[0].heading, DEFAULT_HEADING);
        assert_eq!(oldest.sections[0].items.len(), 1);
    }

    #[test]
    fn version_comparison_is_numeric_rather_than_lexical() {
        assert!(is_newer("0.10.0", "0.9.0"), "10 comes after 9");
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.2.1", "0.2.0"));

        assert!(!is_newer("0.2.0", "0.2.0"), "the same version is not an update");
        assert!(!is_newer("0.1.0", "0.2.0"), "a downgrade is not an update");
        assert!(!is_newer("nonsense", "0.1.0"), "unparseable is not newer than anything");
    }

    #[test]
    fn versions_are_read_the_way_people_write_them() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1.4.2"), Some((1, 4, 2)));
        assert_eq!(parse_version("2.1"), Some((2, 1, 0)));
        assert_eq!(parse_version("0.3.0-beta.1"), Some((0, 3, 0)));

        assert_eq!(parse_version("Unreleased"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn since_returns_only_what_the_user_has_not_seen() {
        let releases = parse(SAMPLE);

        assert_eq!(since(&releases, "0.1.0").len(), 1, "only 0.2.0 is newer");
        assert!(since(&releases, "0.2.0").is_empty(), "nothing is newer than the newest");
        assert!(since(&releases, "9.0.0").is_empty(), "a future version has nothing to catch up on");
        assert_eq!(since(&releases, "0.0.1").len(), 2);
    }

    #[test]
    fn a_single_entry_can_be_looked_up() {
        let releases = parse(SAMPLE);

        assert_eq!(find(&releases, "0.2.0").unwrap().version, "0.2.0");
        assert_eq!(find(&releases, "v0.2.0").unwrap().version, "0.2.0");
        assert!(find(&releases, "0.5.0").is_none());
    }
}
