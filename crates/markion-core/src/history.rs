//! UI-independent portion of Markion's `src/app/state.rs` history machinery.
//!
//! Snapshot compaction, UTF-8-safe diffs, checked restoration, selection restore,
//! and semantic typing/deletion/IME grouping are retained from upstream.
//! Call `push_undo_snapshot` before an atomic document operation, or
//! `prepare_undo_capture` before each coalescible input edit. Finish a capture
//! when moving the caret or changing editing context. Replacing the document
//! requires clearing history (or constructing a new EditorState).

use crate::{
    CheckedMutation, MarkdownDocument, MutationOrigin, MutationReceipt, MutationRejection,
};
use std::{
    ops::Range,
    time::{Duration, Instant},
};

pub const MAX_HISTORY_LEN: usize = 200;
pub const SEMANTIC_UNDO_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoCaptureKind {
    Insert,
    Delete,
    Ime,
    Atomic,
}

#[derive(Clone, Copy, Debug)]
pub struct UndoCapture {
    pub kind: UndoCaptureKind,
    pub last_edit_at: Instant,
    pub next_cursor: usize,
}

#[derive(Clone)]
pub struct EditorSnapshot {
    pub document: MarkdownDocument,
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
}

/// One entry in the undo/redo history.
///
/// Edit sites push `Full` pre-edit snapshots exactly as before, but
/// [`push_history_entry`] compacts the previously-newest full entry into a
/// `Diff` at push time — the only moment both texts are in hand. Each stack
/// therefore retains at most one whole-document copy (its newest entry) no
/// matter how long the history grows; previously a 1 MB document accumulated
/// up to `MAX_HISTORY_LEN` full clones (~200 MB) while typing.
#[allow(clippy::large_enum_variant)]
enum UndoEntry {
    Full(EditorSnapshot),
    Diff(UndoDiff),
}

/// Compact history record. LIFO order guarantees that when this entry is
/// popped the document text is exactly the state the diff was computed
/// against, so applying it means: replace `range` of the current text with
/// `insert`, then restore the recorded selection.
struct UndoDiff {
    pub range: Range<usize>,
    pub insert: String,
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
}

/// Push a history entry onto `stack`, compacting the previous top into a
/// [`UndoDiff`] when both it and the new entry are `Full` (a `Diff` on top is
/// already compact, and its presence means the buried full entry pops against
/// a text we cannot know yet). Caps the stack at [`MAX_HISTORY_LEN`].
fn push_history_entry(stack: &mut Vec<UndoEntry>, entry: UndoEntry) {
    if let (UndoEntry::Full(new), Some(top)) = (&entry, stack.last_mut())
        && let UndoEntry::Full(old) = top
    {
        *top = UndoEntry::Diff(compact_history_entry(old, new.document.text()));
    }
    stack.push(entry);
    if stack.len() > MAX_HISTORY_LEN {
        stack.remove(0);
    }
}

/// Compact `older` (a full snapshot) into a diff against `newer_text`, the
/// state that will be current when the entry is popped: replacing the changed
/// byte range of `newer_text` with the bytes it replaced in `older` restores
/// the older text exactly.
fn compact_history_entry(older: &EditorSnapshot, newer_text: &str) -> UndoDiff {
    let old_text = older.document.text();
    let old_bytes = old_text.as_bytes();
    let new_bytes = newer_text.as_bytes();
    let max_prefix = old_bytes.len().min(new_bytes.len());
    let mut prefix = 0;
    while prefix < max_prefix && old_bytes[prefix] == new_bytes[prefix] {
        prefix += 1;
    }
    let max_suffix = max_prefix - prefix;
    let mut suffix = 0;
    while suffix < max_suffix
        && old_bytes[old_bytes.len() - 1 - suffix] == new_bytes[new_bytes.len() - 1 - suffix]
    {
        suffix += 1;
    }
    // The byte-level bounds may fall inside a UTF-8 sequence when the old and
    // new text share leading/trailing bytes of different chars (e.g. 中 vs 串
    // share two of three bytes); widen to char boundaries in both strings so
    // the stored slices stay valid UTF-8.
    while prefix > 0 && (!old_text.is_char_boundary(prefix) || !newer_text.is_char_boundary(prefix))
    {
        prefix -= 1;
    }
    while suffix > 0
        && (!old_text.is_char_boundary(old_text.len() - suffix)
            || !newer_text.is_char_boundary(newer_text.len() - suffix))
    {
        suffix -= 1;
    }
    UndoDiff {
        range: prefix..newer_text.len() - suffix,
        insert: old_text[prefix..old_text.len() - suffix].to_string(),
        selected_range: older.selected_range.clone(),
        selection_reversed: older.selection_reversed,
    }
}

/// Document and source selection with upstream compact undo/redo history.
///
/// `document` is the original MarkdownDocument, not a separate editor model.
/// Source offsets, selection and composition ranges are UTF-8 bytes.
pub struct EditorState {
    pub document: MarkdownDocument,
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
    pub marked_range: Option<Range<usize>>,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    undo_capture: Option<UndoCapture>,
}

impl EditorState {
    pub fn new(document: MarkdownDocument) -> Self {
        Self {
            document,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_capture: None,
        }
    }

    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }
    pub fn redo_len(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn clear_history(&mut self) {
        self.finish_undo_capture();
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Apply one atomic, checked edit; rejected/no-op edits do not enter history
    /// or discard redo. For coalesced input use prepare_undo_capture instead.
    pub fn apply_checked_mutation(
        &mut self,
        mutation: CheckedMutation,
    ) -> Result<MutationReceipt, MutationRejection> {
        let snapshot = self.snapshot();
        let receipt = self.document.apply_checked_mutation(mutation)?;
        if receipt.changed {
            self.commit_undo_snapshot(snapshot);
        }
        Ok(receipt)
    }

    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            document: self.document.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    pub fn push_undo_snapshot(&mut self) {
        self.finish_undo_capture();
        let snapshot = self.snapshot();
        push_history_entry(&mut self.undo_stack, UndoEntry::Full(snapshot));
        self.redo_stack.clear();
    }

    pub fn commit_undo_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.finish_undo_capture();
        push_history_entry(&mut self.undo_stack, UndoEntry::Full(snapshot));
        self.redo_stack.clear();
    }

    pub fn finish_undo_capture(&mut self) {
        self.undo_capture = None;
    }

    pub fn prepare_undo_capture(
        &mut self,
        kind: UndoCaptureKind,
        range: &Range<usize>,
        replacement: &str,
        now: Instant,
    ) {
        let compatible = self.undo_capture.is_some_and(|capture| {
            capture.kind == kind
                && !matches!(kind, UndoCaptureKind::Atomic)
                && now.saturating_duration_since(capture.last_edit_at) <= SEMANTIC_UNDO_TIMEOUT
                && match kind {
                    UndoCaptureKind::Insert => {
                        range.is_empty() && range.start == capture.next_cursor
                    }
                    UndoCaptureKind::Delete => {
                        replacement.is_empty()
                            && (range.start == capture.next_cursor
                                || range.end == capture.next_cursor)
                    }
                    UndoCaptureKind::Ime => true,
                    UndoCaptureKind::Atomic => false,
                }
        });
        if !compatible {
            self.finish_undo_capture();
            let snapshot = self.snapshot();
            push_history_entry(&mut self.undo_stack, UndoEntry::Full(snapshot));
            self.redo_stack.clear();
        }
        if matches!(kind, UndoCaptureKind::Atomic) {
            self.undo_capture = None;
        } else {
            self.undo_capture = Some(UndoCapture {
                kind,
                last_edit_at: now,
                next_cursor: range.start + replacement.len(),
            });
        }
    }

    /// Restore a full snapshot's document text and selection through the
    /// checked mutation boundary on the live document. A wholesale
    /// `self.document = snapshot.document` assignment would resurrect the
    /// snapshot's older `text_version`, and a version number that names two
    /// different texts poisons every tab-level per-version cache (display
    /// text, source layout, measured height) that assumes versions are
    /// globally unique. Restore only the changed UTF-8-safe range so unchanged
    /// visual rows retain their source lineage and the list's scroll anchor.
    /// A whole-document mutation would discard all row identities and splice
    /// the entire list, moving its anchor to row zero. The checked range edit
    /// keeps instance identity and monotonic versions. Returns false when the
    /// boundary rejects restoration.
    pub fn restore_snapshot(&mut self, snapshot: EditorSnapshot, origin: MutationOrigin) -> bool {
        if snapshot.document.instance_id() != self.document.instance_id() {
            return false;
        }
        let diff = compact_history_entry(&snapshot, self.document.text());
        self.apply_history_diff(diff, origin).is_some()
    }

    /// Apply a compact history record and return its inverse — the record
    /// that, pushed onto the opposite stack, re-creates the state being left.
    /// Returns `None` when the record no longer matches the live text: the
    /// checked boundary rejected the splice and nothing changed.
    fn apply_history_diff(&mut self, diff: UndoDiff, origin: MutationOrigin) -> Option<UndoDiff> {
        let expected = self.document.text().get(diff.range.clone())?.to_string();
        let inverse = UndoDiff {
            range: diff.range.start..diff.range.start + diff.insert.len(),
            insert: expected,
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        };
        let mutation = self
            .document
            .prepare_range_mutation(origin, diff.range, diff.insert);
        self.document.apply_checked_mutation(mutation).ok()?;
        self.selected_range = diff.selected_range;
        self.selection_reversed = diff.selection_reversed;
        self.marked_range = None;
        Some(inverse)
    }

    /// Pop and apply the newest undo entry, pushing its inverse onto the redo
    /// stack. Returns false when there is nothing to undo.
    pub fn apply_undo(&mut self) -> bool {
        self.finish_undo_capture();
        let Some(entry) = self.undo_stack.pop() else {
            return false;
        };
        match entry {
            UndoEntry::Full(snapshot) => {
                let current = self.snapshot();
                if self.restore_snapshot(snapshot, MutationOrigin::Undo) {
                    push_history_entry(&mut self.redo_stack, UndoEntry::Full(current));
                }
            }
            UndoEntry::Diff(diff) => {
                if let Some(inverse) = self.apply_history_diff(diff, MutationOrigin::Undo) {
                    push_history_entry(&mut self.redo_stack, UndoEntry::Diff(inverse));
                }
            }
        }
        true
    }

    /// Pop and apply the newest redo entry, pushing its inverse onto the undo
    /// stack (without clearing redo). Returns false when there is nothing to
    /// redo.
    pub fn apply_redo(&mut self) -> bool {
        self.finish_undo_capture();
        let Some(entry) = self.redo_stack.pop() else {
            return false;
        };
        match entry {
            UndoEntry::Full(snapshot) => {
                let current = self.snapshot();
                if self.restore_snapshot(snapshot, MutationOrigin::Redo) {
                    push_history_entry(&mut self.undo_stack, UndoEntry::Full(current));
                }
            }
            UndoEntry::Diff(diff) => {
                if let Some(inverse) = self.apply_history_diff(diff, MutationOrigin::Redo) {
                    push_history_entry(&mut self.undo_stack, UndoEntry::Diff(inverse));
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_history_keeps_one_full_snapshot_and_compacts_the_rest() {
        let mut tab = EditorState::new(MarkdownDocument::from_text("hello world"));
        for (range, insert) in [(5..5, ","), (12..12, "!"), (0..5, "goodbye")] {
            tab.selected_range = range.start..range.start;
            tab.push_undo_snapshot();
            tab.document.replace_range(range, insert);
        }

        assert_eq!(tab.undo_stack.len(), 3);
        assert!(matches!(tab.undo_stack[0], UndoEntry::Diff(_)));
        assert!(matches!(tab.undo_stack[1], UndoEntry::Diff(_)));
        assert!(matches!(tab.undo_stack[2], UndoEntry::Full(_)));
    }

    #[test]
    fn undo_redo_roundtrip_through_compacted_history() {
        // Walk a document through edits that exercise insertion, deletion,
        // replacement, and multi-byte chars sharing UTF-8 prefix bytes
        // (中 E4B8AD vs 串 E4B8B2 — the byte diff lands mid-char and must be
        // widened to a char boundary), then undo/redo across the whole
        // history and verify every intermediate state.
        let edits: [(Range<usize>, &str); 4] =
            [(5..5, " 中"), (9..9, " beta"), (5..9, " 串"), (0..5, "")];
        let mut tab = EditorState::new(MarkdownDocument::from_text("alpha"));
        // Undoing edit i restores its pre-edit text and the selection captured
        // when its snapshot was pushed (the range about to be replaced).
        let mut undo_expected: Vec<(String, Range<usize>)> = Vec::new();
        let mut redo_texts: Vec<String> = Vec::new();
        for (range, insert) in edits {
            undo_expected.push((tab.document.text().to_string(), range.clone()));
            tab.selected_range = range.clone();
            tab.push_undo_snapshot();
            tab.document.replace_range(range.clone(), insert);
            let end = range.start + insert.len();
            tab.selected_range = end..end;
            redo_texts.push(tab.document.text().to_string());
        }

        // Undo all the way down, checking text and selection at every step.
        for (text, selection) in undo_expected.iter().rev() {
            assert!(tab.apply_undo());
            assert_eq!(tab.document.text(), text, "undo text");
            assert_eq!(&tab.selected_range, selection, "undo selection");
        }
        assert!(!tab.apply_undo());

        // Redo all the way back up.
        for text in &redo_texts {
            assert!(tab.apply_redo());
            assert_eq!(tab.document.text(), text, "redo text");
        }
        assert!(!tab.apply_redo());

        // Interleave: undo twice, redo once, then a fresh edit clears redo.
        assert!(tab.apply_undo());
        assert!(tab.apply_undo());
        assert!(tab.apply_redo());
        assert_eq!(tab.document.text(), redo_texts[2]);
        tab.push_undo_snapshot();
        tab.document.replace_range(0..0, "x");
        assert!(tab.redo_stack.is_empty());
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), redo_texts[2]);
    }

    #[test]
    fn semantic_undo_coalesces_typing_and_deletion_but_respects_boundaries() {
        let now = Instant::now();
        let mut tab = EditorState::new(MarkdownDocument::from_text(""));
        for (index, text) in ["你", "好"].into_iter().enumerate() {
            let range = tab.document.text().len()..tab.document.text().len();
            tab.prepare_undo_capture(
                UndoCaptureKind::Insert,
                &range,
                text,
                now + Duration::from_millis(index as u64 * 100),
            );
            tab.document.replace_range(range.clone(), text);
            let cursor = range.start + text.len();
            tab.selected_range = cursor..cursor;
        }
        assert_eq!(tab.undo_stack.len(), 1);
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), "");
        assert!(tab.apply_redo());
        assert_eq!(tab.document.text(), "你好");

        tab.finish_undo_capture();
        tab.undo_stack.clear();
        tab.redo_stack.clear();
        let first = "你好".find('好').unwrap();
        for (index, range) in [first.."你好".len(), 0..first].into_iter().enumerate() {
            tab.prepare_undo_capture(
                UndoCaptureKind::Delete,
                &range,
                "",
                now + Duration::from_millis(index as u64 * 100),
            );
            tab.document.replace_range(range.clone(), "");
            tab.selected_range = range.start..range.start;
        }
        assert_eq!(tab.undo_stack.len(), 1);
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), "你好");

        tab.undo_stack.clear();
        tab.redo_stack.clear();
        tab.document.set_text("a");
        tab.selected_range = 1..1;
        tab.prepare_undo_capture(UndoCaptureKind::Insert, &(1..1), "b", now);
        tab.document.replace_range(1..1, "b");
        tab.selected_range = 2..2;
        tab.prepare_undo_capture(
            UndoCaptureKind::Insert,
            &(2..2),
            "c",
            now + SEMANTIC_UNDO_TIMEOUT + Duration::from_millis(1),
        );
        tab.document.replace_range(2..2, "c");
        tab.selected_range = 3..3;
        assert_eq!(tab.undo_stack.len(), 2, "timeout starts a new undo group");
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), "ab");
    }

    #[test]
    fn semantic_undo_keeps_selection_replacement_and_atomic_commands_separate() {
        let now = Instant::now();
        let mut tab = EditorState::new(MarkdownDocument::from_text("alpha"));
        tab.selected_range = 0..5;
        tab.prepare_undo_capture(UndoCaptureKind::Atomic, &(0..5), "x", now);
        tab.document.replace_range(0..5, "x");
        tab.selected_range = 1..1;
        tab.prepare_undo_capture(
            UndoCaptureKind::Insert,
            &(1..1),
            "y",
            now + Duration::from_millis(10),
        );
        tab.document.replace_range(1..1, "y");
        tab.selected_range = 2..2;

        assert_eq!(tab.undo_stack.len(), 2);
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), "x");
        assert!(tab.apply_undo());
        assert_eq!(tab.document.text(), "alpha");
    }
}
