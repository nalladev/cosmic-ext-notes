// SPDX-License-Identifier: MPL-2.0

//! Wikilink (`[[...]]`) indexing across the vault.
//!
//! A note can reference another note by writing `[[Title]]` (or
//! `[[Title|display text]]`) anywhere in its body. This module extracts those
//! references, resolves them to vault notes, and answers the reverse
//! question — "which notes link to this one?" — as backlinks.
//!
//! It is pure logic over the public [`Vault`] API, free of UI dependencies,
//! so it can be unit-tested in isolation. The application rebuilds the
//! [`BacklinkIndex`] whenever the vault changes and renders the results of
//! [`BacklinkIndex::for_note`] for each open note.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::notes::{Note, Vault};

/// A single `[[...]]` reference to another note, found in one note's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// Path of the note that contains the reference.
    pub from: PathBuf,
    /// Path of the note the reference resolves to.
    pub to: PathBuf,
    /// The raw link text, including any `|display` alias.
    pub target: String,
    /// The 1-based line of the referencing note's body the link is on.
    pub line: usize,
    /// The trimmed text of that line, as display context.
    pub context: String,
}

/// Extracts the raw `[[...]]` link targets from `text`, in document order.
///
/// Targets may carry a display alias (`[[Title|see here]]`) and are trimmed.
/// Content inside fenced code blocks is skipped, so `[[example]]` in a code
/// sample is not treated as a link.
#[must_use]
pub fn extract_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("[[") {
            let after = &rest[start + 2..];
            if let Some(end) = after.find("]]") {
                links.push(after[..end].trim().to_owned());
                rest = &after[end + 2..];
            } else {
                break;
            }
        }
    }
    links
}

/// The canonical form of a link target: whitespace trimmed, any `|display`
/// alias dropped, and a trailing `.md` extension stripped. `[[Note.md|see]]`
/// canonicalizes to `Note`.
#[must_use]
pub fn canonical_target(target: &str) -> String {
    let base = target.split('|').next().unwrap_or(target).trim();
    base.strip_suffix(".md").unwrap_or(base).to_owned()
}

/// Resolves a link target to one of `notes`: an exact title match first, then
/// a case-insensitive title match, then a path-suffix match (for
/// `[[subdir/note]]` style targets, once the vault scans nested notes).
fn resolve<'a>(notes: &[&'a Note], target: &str) -> Option<&'a Note> {
    let canonical = canonical_target(target);
    notes
        .iter()
        .copied()
        .find(|note| note.title() == canonical)
        .or_else(|| {
            let want = canonical.to_lowercase();
            notes
                .iter()
                .copied()
                .find(|note| note.title().to_lowercase() == want)
        })
        .or_else(|| {
            let as_path = Path::new(target);
            notes.iter().copied().find(|note| {
                note.path().ends_with(as_path)
                    || note.path().ends_with(as_path.with_extension("md"))
            })
        })
}

/// A lookup table of backlinks across the vault, rebuilt from a [`Vault`].
///
/// Build once per vault mutation; the results are deterministic — the
/// backlinks of a note are sorted by referencing note path, then by line.
#[derive(Debug, Default)]
pub struct BacklinkIndex {
    by_target: HashMap<PathBuf, Vec<Backlink>>,
}

impl BacklinkIndex {
    /// Rebuilds the index from every non-trashed note in the vault.
    #[must_use]
    pub fn build(vault: &Vault) -> Self {
        let notes: Vec<&Note> = vault
            .notes()
            .iter()
            .filter(|note| !note.in_trash())
            .collect();
        let mut index = Self::default();
        for note in &notes {
            index.add_note(&notes, note);
        }
        for backlinks in index.by_target.values_mut() {
            backlinks.sort_by(|a, b| a.from.cmp(&b.from).then(a.line.cmp(&b.line)));
        }
        index
    }

    /// The backlinks of the note at `path`, sorted by referencing note and
    /// line. Empty when the note has no backlinks (or is not in the vault).
    #[must_use]
    pub fn for_note(&self, path: &Path) -> &[Backlink] {
        self.by_target.get(path).map_or(&[], Vec::as_slice)
    }

    fn add_note(&mut self, notes: &[&Note], note: &Note) {
        let from = note.path().to_path_buf();
        for (line, context) in note.body().lines().enumerate() {
            for target in extract_links(context) {
                let Some(to) = resolve(notes, &target) else {
                    continue;
                };
                if to.path() == from {
                    // A note does not backlink itself.
                    continue;
                }
                self.by_target
                    .entry(to.path().to_path_buf())
                    .or_default()
                    .push(Backlink {
                        from: from.clone(),
                        to: to.path().to_path_buf(),
                        target,
                        line: line + 1,
                        context: context.trim().to_owned(),
                    });
            }
        }
    }
}

/// The backlinks of the note at `path` in `vault`: every non-trashed note
/// whose body links to it, sorted by note then line.
#[must_use]
pub fn backlinks(vault: &Vault, path: &Path) -> Vec<Backlink> {
    BacklinkIndex::build(vault).for_note(path).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Creates a fresh, empty temporary directory for a test.
    fn temp_dir() -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cosmic-notes-backlinks-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    fn open_vault() -> Vault {
        Vault::open(temp_dir().join("vault")).expect("vault should open")
    }

    /// Creates a note with the given body and returns its path.
    fn save(vault: &mut Vault, title: &str, body: &str) -> PathBuf {
        let note = vault.create(title).expect("create").expect("title");
        vault.save(note.path(), body).expect("save").expect("saved");
        note.path().to_path_buf()
    }

    #[test]
    fn extract_links_finds_targets_in_document_order() {
        assert_eq!(
            extract_links("See [[Alpha]] and [[Beta|the beta note]] here."),
            vec!["Alpha".to_owned(), "Beta|the beta note".to_owned()]
        );
        assert_eq!(extract_links("No links here"), Vec::<String>::new());
    }

    #[test]
    fn extract_links_ignores_unclosed_targets() {
        assert_eq!(extract_links("stray [[Alpha"), Vec::<String>::new());
    }

    #[test]
    fn extract_links_skips_fenced_code() {
        let text = "Before\n```\n[[NotALink]]\n```\nAfter [[RealLink]]";
        assert_eq!(extract_links(text), vec!["RealLink".to_owned()]);
    }

    #[test]
    fn canonical_target_strips_alias_and_extension() {
        assert_eq!(canonical_target(" Note "), "Note");
        assert_eq!(canonical_target("Note|see here"), "Note");
        assert_eq!(canonical_target("Note.md"), "Note");
        assert_eq!(canonical_target("Note.md|see"), "Note");
    }

    #[test]
    fn backlinks_resolve_case_insensitively() {
        let mut vault = open_vault();
        let beta = save(&mut vault, "Beta", "# Beta\n\nContent");
        save(&mut vault, "Alpha", "Links to [[beta]] and nothing else.");

        let links = backlinks(&vault, &beta);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "beta");
        assert_eq!(links[0].line, 1);
        assert_eq!(links[0].context, "Links to [[beta]] and nothing else.");
    }

    #[test]
    fn backlinks_include_alias_and_extension_targets() {
        let mut vault = open_vault();
        let target = save(&mut vault, "Roadmap", "# Roadmap");
        save(&mut vault, "Ideas", "See [[Roadmap.md|current plan]].");

        let links = backlinks(&vault, &target);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Roadmap.md|current plan");
    }

    #[test]
    fn backlinks_exclude_self_links_and_trashed_notes() {
        let mut vault = open_vault();
        let note = save(&mut vault, "Solo", "I link to [[Solo]].");

        // A self-link is not a backlink.
        assert!(backlinks(&vault, &note).is_empty());

        // A trashed note's links do not count.
        let target = save(&mut vault, "Target", "# Target");
        let source = save(&mut vault, "Source", "Links [[Target]]");
        assert_eq!(backlinks(&vault, &target).len(), 1);
        vault.trash(&source).expect("trash");
        assert!(backlinks(&vault, &target).is_empty());
    }

    #[test]
    fn index_is_sorted_by_note_then_line() {
        let mut vault = open_vault();
        let target = save(&mut vault, "Target", "# Target");
        let first = save(
            &mut vault,
            "Aardvark",
            "First [[Target]]\nSecond line\nThird [[Target]]",
        );
        let second = save(&mut vault, "Zebra", "[[Target]]");

        let index = BacklinkIndex::build(&vault);
        let links = index.for_note(&target);

        // Both referencing notes, in title order.
        let mut paths: Vec<&Path> = links.iter().map(|l| l.from.as_path()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![first.as_path(), first.as_path(), second.as_path()]
        );

        // Document order within a note: line 1 before line 3.
        let first_note_lines: Vec<usize> = links
            .iter()
            .filter(|l| l.from == first)
            .map(|l| l.line)
            .collect();
        assert_eq!(first_note_lines, vec![1, 3]);
    }
}
