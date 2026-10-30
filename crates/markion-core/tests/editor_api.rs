//! Consumer-side coverage for the extraction's public API and history adapter.

use markion_core::{
    CheckedMutation, EditorState, MarkdownDocument, MutationOrigin, MutationRejectionReason,
    VisualCaretAffinity, build_visual_projection_with_marked_range,
};

#[test]
fn projected_unicode_edit_changes_canonical_source_and_roundtrips_history() {
    let mut editor = EditorState::new(MarkdownDocument::from_text("plain **中文** end\n"));
    let blocks = editor.document.visual_blocks_shared();
    let projection = build_visual_projection_with_marked_range(
        editor.document.text(),
        &blocks[0],
        0..0,
        0,
        None,
        false,
    );
    assert_eq!(projection.text, "plain 中文 end");
    let display = projection.text.find('文').unwrap();
    let source =
        projection.source_for_display_with_affinity(display, VisualCaretAffinity::Downstream);
    assert_eq!(
        &editor.document.text()[source..source + '文'.len_utf8()],
        "文"
    );
    assert_eq!(projection.display_for_source(source), Some(display));

    editor.selected_range = source..source + '文'.len_utf8();
    let edit = editor.document.prepare_range_mutation(
        MutationOrigin::PlatformTextInput,
        editor.selected_range.clone(),
        "字",
    );
    editor.apply_checked_mutation(edit).unwrap();
    editor.selected_range = source + '字'.len_utf8()..source + '字'.len_utf8();
    assert_eq!(editor.document.text(), "plain **中字** end\n");
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.apply_undo());
    assert_eq!(editor.document.text(), "plain **中文** end\n");
    assert_eq!(editor.selected_range, source..source + '文'.len_utf8());
    assert!(editor.apply_redo());
    assert_eq!(editor.document.text(), "plain **中字** end\n");
}

#[test]
fn atomic_rejections_and_noops_preserve_redo_and_source() {
    let mut editor = EditorState::new(MarkdownDocument::from_text("hello"));
    let edit = editor
        .document
        .prepare_range_mutation(MutationOrigin::PlatformTextInput, 5..5, "!");
    editor.apply_checked_mutation(edit).unwrap();
    assert!(editor.apply_undo());
    assert_eq!((editor.undo_len(), editor.redo_len()), (0, 1));

    let invalid =
        editor
            .document
            .prepare_range_mutation(MutationOrigin::PlatformTextInput, 99..99, "x");
    assert_eq!(
        editor.apply_checked_mutation(invalid).unwrap_err().reason,
        MutationRejectionReason::OutOfBounds
    );
    let noop =
        editor
            .document
            .prepare_range_mutation(MutationOrigin::PlatformTextInput, 0..5, "hello");
    assert!(!editor.apply_checked_mutation(noop).unwrap().changed);
    assert_eq!(editor.document.text(), "hello");
    assert_eq!((editor.undo_len(), editor.redo_len()), (0, 1));
    assert!(editor.apply_redo());
    assert_eq!(editor.document.text(), "hello!");
}

#[test]
fn undo_redo_keeps_identity_monotonic_versions_and_checked_origins() {
    let mut editor = EditorState::new(MarkdownDocument::from_text("before\n\nuntouched\n"));
    let id = editor.document.instance_id();
    let original_version = editor.document.version();
    let stale =
        editor
            .document
            .prepare_range_mutation(MutationOrigin::PlatformTextInput, 0..0, "stale");
    let edit =
        editor
            .document
            .prepare_range_mutation(MutationOrigin::PlatformTextInput, 0..6, "after");
    editor.apply_checked_mutation(edit).unwrap();
    let edited_version = editor.document.version();
    assert!(editor.apply_undo());
    let undo_version = editor.document.version();
    assert!(editor.apply_redo());
    assert!(original_version < edited_version && edited_version < undo_version);
    assert!(undo_version < editor.document.version());
    assert_eq!(editor.document.instance_id(), id);
    assert_eq!(
        editor.apply_checked_mutation(stale).unwrap_err().reason,
        MutationRejectionReason::StaleVersion
    );
    let accepted: Vec<_> = editor
        .document
        .mutation_journal()
        .into_iter()
        .filter(|entry| entry.rejection.is_none())
        .map(|entry| entry.origin)
        .collect();
    assert_eq!(
        accepted,
        [
            MutationOrigin::PlatformTextInput,
            MutationOrigin::Undo,
            MutationOrigin::Redo
        ]
    );
}

#[test]
fn direct_visual_field_edit_is_checked_and_undoable() {
    let mut editor = EditorState::new(MarkdownDocument::from_text("```rust\nlet 名 = 1;\n```\n"));
    let range = editor.document.text().find('1').unwrap();
    let edit = editor
        .document
        .direct_visual_block_edit(range..range + 1, "2")
        .unwrap();
    assert!(editor.document.validate_visual_block_edit(&edit));
    let mutation = CheckedMutation::range(
        editor.document.instance_id(),
        edit.document_version,
        MutationOrigin::ExactBlockEdit,
        edit.range.clone(),
        &editor.document.text()[edit.range.clone()],
        edit.replacement,
    );
    editor.apply_checked_mutation(mutation).unwrap();
    editor.selected_range = edit.selection_after;
    assert_eq!(editor.document.text(), "```rust\nlet 名 = 2;\n```\n");
    assert!(editor.apply_undo());
    assert_eq!(editor.document.text(), "```rust\nlet 名 = 1;\n```\n");
}

#[test]
fn snapshots_cannot_restore_into_a_different_document() {
    let one = EditorState::new(MarkdownDocument::from_text("one"));
    let mut two = EditorState::new(MarkdownDocument::from_text("two"));
    assert!(!two.restore_snapshot(one.snapshot(), MutationOrigin::Undo));
    assert_eq!(two.document.text(), "two");
}
