use super::widget::Harness;
use super::*;
use cosmic::iced::{
    Event,
    core::{input_method, mouse},
    keyboard::{Key, Modifiers, key::Named},
};

fn named(harness: &mut Harness, editor: &mut Editor, key: Named) {
    harness.key(editor, Key::Named(key), None, Modifiers::empty());
}
fn type_text(harness: &mut Harness, editor: &mut Editor, text: &str) {
    for ch in text.chars() {
        let text = ch.to_string();
        harness.key(
            editor,
            Key::Character(text.clone().into()),
            Some(&text),
            Modifiers::empty(),
        );
    }
}
fn select(editor: &mut Editor, start: usize, end: usize) {
    editor.update(Message::Select {
        anchor: start,
        cursor: end,
    });
}

#[test]
fn construction_preserves_authored_bytes_and_renders_inline_styles() {
    let source =
        "# Title\n\nA **bold** *italic* ~~gone~~ `code` [link](https://example.org) &amp; 中\n";
    let editor = Editor::new(source);
    assert_eq!(editor.text(), source);
    let display = &editor.surface.projection.text;
    assert!(
        display.contains("A bold italic gone code link & 中"),
        "{display:?}"
    );
    assert!(!display.contains("https://"));
    assert!(editor.surface.projection.spans.iter().any(|s| s.style.bold));
    assert!(
        editor
            .surface
            .projection
            .spans
            .iter()
            .any(|s| s.style.italic)
    );
    assert!(
        editor
            .surface
            .projection
            .spans
            .iter()
            .any(|s| s.style.strikethrough)
    );
    assert!(editor.surface.projection.spans.iter().any(|s| s.style.code));
    assert!(editor.surface.projection.spans.iter().any(|s| s.link));
    assert_eq!(editor.surface.rows[0].cells[0].presentation.size, 27.0);
}

#[test]
fn native_pointer_hit_types_into_hidden_bold_and_undo_restores_selection() {
    let mut editor = Editor::new("before **bold** after");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 11); // bo|ld in canonical source
    assert_eq!(editor.caret(), 11);
    type_text(&mut harness, &mut editor, "XY");
    assert_eq!(editor.text(), "before **boXYld** after");
    assert_eq!(editor.state.undo_len(), 1, "semantic typing capture");
    named(&mut harness, &mut editor, Named::ArrowLeft);
    editor.undo();
    assert_eq!(editor.text(), "before **bold** after");
    assert_eq!(editor.caret(), 11);
    editor.redo();
    assert_eq!(editor.text(), "before **boXYld** after");
}

#[test]
fn native_drag_selects_across_blocks_and_draws_multiple_highlights() {
    let mut editor = Editor::new("first **bold**\n\nsecond *italic*");
    let mut harness = Harness::new(&editor, 300.0, 300.0);
    let from = harness.point(&editor, 2);
    let to = harness.point(&editor, 20);
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Some(from),
    );
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::CursorMoved { position: to }),
        Some(to),
    );
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        Some(to),
    );
    assert_eq!(editor.selection(), 2..20);
    assert!(harness.native_highlights(&editor) >= 2);
    let expected = format!("{}X{}", &editor.text()[..2], &editor.text()[20..]);
    type_text(&mut harness, &mut editor, "X");
    assert_eq!(editor.text(), expected);
    editor.undo();
    assert_eq!(editor.selection(), 2..20);
}

#[test]
fn keyboard_cross_block_selection_copy_cut_paste_and_history() {
    let mut editor = Editor::new("**one**\n\ntwo");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 3);
    harness.key(
        &mut editor,
        Key::Named(Named::End),
        None,
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    assert_eq!(editor.selection().end, editor.text().len());
    harness.key(
        &mut editor,
        Key::Character("c".into()),
        None,
        Modifiers::CTRL,
    );
    assert!(harness.clipboard().unwrap().contains("two"));
    harness.key(
        &mut editor,
        Key::Character("x".into()),
        None,
        Modifiers::CTRL,
    );
    assert!(!editor.text().contains("two"));
    harness.key(
        &mut editor,
        Key::Character("z".into()),
        None,
        Modifiers::CTRL,
    );
    assert_eq!(editor.text(), "**one**\n\ntwo");
    harness.set_clipboard("new\ntext");
    harness.key(
        &mut editor,
        Key::Character("v".into()),
        None,
        Modifiers::CTRL,
    );
    assert!(editor.text().contains("new\ntext"));
}

#[test]
fn grapheme_backspace_and_delete_use_native_navigation() {
    for grapheme in ["e\u{301}", "👩\u{200d}💻", "🇫🇮", "中"] {
        let source = format!("a{grapheme}z");
        let mut editor = Editor::new(&source);
        let mut harness = Harness::new(&editor, 640.0, 320.0);
        harness.click(&mut editor, 1 + grapheme.len());
        named(&mut harness, &mut editor, Named::Backspace);
        assert_eq!(editor.text(), "az", "backspace {grapheme}");
        editor.undo();
        select(&mut editor, 1, 1);
        named(&mut harness, &mut editor, Named::Delete);
        assert_eq!(editor.text(), "az", "delete {grapheme}");
    }
}

#[test]
fn decoded_entity_is_an_atomic_source_edit() {
    let mut editor = Editor::new("a &amp; b");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 7);
    named(&mut harness, &mut editor, Named::Backspace);
    assert_eq!(editor.text(), "a  b");
    editor.undo();
    assert_eq!(editor.text(), "a &amp; b");
}

#[test]
fn native_ime_preedit_cancel_and_commit_are_source_backed() {
    let mut editor = Editor::new("**old**");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 3);
    select(&mut editor, 2, 5);
    harness.event(
        &mut editor,
        Event::InputMethod(input_method::Event::Opened),
        None,
    );
    harness.event(
        &mut editor,
        Event::InputMethod(input_method::Event::Preedit("に".into(), Some(3..3))),
        None,
    );
    assert_eq!(editor.text(), "**old**");
    named(&mut harness, &mut editor, Named::Backspace); // composing keys must not mutate source
    assert_eq!(editor.text(), "**old**");
    harness.event(
        &mut editor,
        Event::InputMethod(input_method::Event::Closed),
        None,
    );
    assert!(!editor.can_undo());
    harness.event(
        &mut editor,
        Event::InputMethod(input_method::Event::Commit("日本語".into())),
        None,
    );
    assert_eq!(editor.text(), "**日本語**");
    assert_eq!(editor.state.undo_len(), 1);
    assert_eq!(
        editor
            .state
            .document
            .mutation_journal()
            .last()
            .unwrap()
            .origin,
        MutationOrigin::ImeComposition
    );
    editor.undo();
    assert_eq!(editor.selection(), 2..5);
    assert_eq!(editor.text(), "**old**");
}

#[test]
fn structural_enter_backspace_and_task_toggle_reuse_core() {
    let mut editor = Editor::new("- item");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 6);
    named(&mut harness, &mut editor, Named::Enter);
    assert_eq!(editor.text(), "- item\n- ");
    named(&mut harness, &mut editor, Named::Enter);
    assert_eq!(editor.text(), "- item\n");
    let mut editor = Editor::new("# heading");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 2);
    named(&mut harness, &mut editor, Named::Backspace);
    assert_eq!(editor.text(), "heading");
    let mut editor = Editor::new("- [ ] task");
    editor.update(Message::ToggleTask);
    assert_eq!(editor.text(), "- [x] task");
    editor.undo();
    assert_eq!(editor.text(), "- [ ] task");
}

#[test]
fn native_table_cell_edit_preserves_grid_and_uses_upstream_reflow() {
    let source = "| Name | Value |\n| :--- | ---: |\n| **one** | two |\n";
    let mut editor = Editor::new(source);
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    assert_eq!(
        editor
            .surface
            .rows
            .iter()
            .filter(|r| r.cells.len() == 2)
            .count(),
        2
    );
    let source_offset = source.find("one").unwrap() + 1;
    harness.click(&mut editor, source_offset);
    type_text(&mut harness, &mut editor, "X");
    assert!(editor.text().contains("**oXne**"), "{}", editor.text());
    assert!(editor.text().contains(":---"));
    assert!(editor.text().contains("---:"));
    named(&mut harness, &mut editor, Named::Tab);
    let field = editor
        .state
        .document
        .visual_editor_field_at(&editor.selection())
        .unwrap();
    assert!(matches!(
        field.kind,
        markion_core::model::VisualEditorFieldKind::TableCell { row: 1, column: 1 }
    ));
    editor.undo();
    assert_eq!(editor.text(), source);
}

#[test]
fn code_payload_edits_keep_authored_fences_and_info() {
    let source = "~~~~rust custom\nlet x = 1;\n~~~~\n";
    let mut editor = Editor::new(source);
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    assert!(!editor.surface.projection.text.contains("~~~~"));
    harness.click(&mut editor, source.find("x =").unwrap() + 1);
    type_text(&mut harness, &mut editor, "y");
    assert_eq!(editor.text(), "~~~~rust custom\nlet xy = 1;\n~~~~\n");
    editor.undo();
    assert_eq!(editor.text(), source);
}

#[test]
fn format_toolbar_and_keyboard_mutate_markdown_not_display() {
    let mut editor = Editor::new("hello world");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 2);
    select(&mut editor, 0, 5);
    harness.key(
        &mut editor,
        Key::Character("b".into()),
        None,
        Modifiers::CTRL,
    );
    assert_eq!(editor.text(), "**hello** world");
    assert_eq!(editor.surface.projection.text, "hello world");
    editor.update(Message::Format(MarkdownFormat::Heading(2)));
    assert!(editor.text().starts_with("## "));
    editor.undo();
    editor.undo();
    assert_eq!(editor.text(), "hello world");
}

#[test]
fn source_islands_and_image_source_toggle_are_editable() {
    let source = "![alt](picture.png)\n\n<div>unknown</div>\n";
    let mut editor = Editor::new(source);
    let mut harness = Harness::new(&editor, 640.0, 500.0);
    assert!(editor.surface.projection.text.contains("▧ alt"));
    editor.update(Message::ToggleSource);
    assert!(
        editor
            .surface
            .projection
            .text
            .contains("![alt](picture.png)")
    );
    harness.click(&mut editor, 3);
    type_text(&mut harness, &mut editor, "X");
    assert!(editor.text().contains("![aXlt]"));
    editor.update(Message::ToggleSource);
    let offset = editor.text().find("unknown").unwrap() + 2;
    harness.click(&mut editor, offset);
    type_text(&mut harness, &mut editor, "Y");
    assert!(editor.text().contains("unYknown"));
}

#[test]
fn stale_hits_noop_redo_and_checked_journal() {
    let mut editor = Editor::new("hello");
    let version = editor.state.document.version();
    editor.update(Message::Insert("X".into()));
    let selection = editor.selection();
    editor.update(Message::Hit {
        version,
        revision: 0,
        display: 4,
        extend: false,
        word: false,
        affinity: VisualCaretAffinity::Downstream,
    });
    assert_eq!(editor.selection(), selection);
    editor.undo();
    select(&mut editor, 0, 0);
    editor.update(Message::Action(Action::Edit(Edit::Backspace)));
    assert!(editor.can_redo());
    assert!(
        editor
            .state
            .document
            .mutation_journal()
            .iter()
            .all(|entry| entry.rejection.is_none())
    );
}

#[test]
fn wrapped_vertical_navigation_and_caret_follow_use_native_geometry() {
    let source = (0..40)
        .map(|i| format!("line {i} with enough words to wrap into several lines\n"))
        .collect::<String>();
    let mut editor = Editor::new(&source);
    let mut harness = Harness::new(&editor, 180.0, 130.0);
    harness.click(&mut editor, 4);
    let before = editor.caret();
    named(&mut harness, &mut editor, Named::ArrowDown);
    assert!(editor.caret() > before);
    harness.key(&mut editor, Key::Named(Named::End), None, Modifiers::CTRL);
    assert_eq!(editor.caret(), editor.text().len());
    assert!(harness.scroll() > 0.0);
}

#[test]
fn empty_and_authored_blank_lines_keep_every_terminal_caret_position() {
    for source in [
        "",
        "\n",
        "\n\n",
        "a\n",
        "a\n\n",
        "a\n\n\n",
        "a\n\nb",
        "\n\nb",
        "a\r\n\r\nb",
    ] {
        let mut editor = Editor::new(source);
        assert_eq!(editor.text(), source);
        let mut harness = Harness::new(&editor, 640.0, 320.0);
        harness.click(&mut editor, 0);
        harness.key(&mut editor, Key::Named(Named::End), None, Modifiers::CTRL);
        assert_eq!(editor.caret(), source.len(), "terminal caret in {source:?}");
        type_text(&mut harness, &mut editor, "X");
        assert_eq!(editor.text(), format!("{source}X"));
        editor.undo();
        assert_eq!(editor.text(), source);
    }
}

#[test]
fn empty_table_cells_and_block_looking_cell_text_stay_inline_editable() {
    let source = "| H | I |\n| --- | --- |\n|  | # literal |\n";
    let mut editor = Editor::new(source);
    assert!(editor.surface.projection.text.contains("# literal"));
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    let cells = editor
        .state
        .document
        .visual_blocks_shared()
        .iter()
        .find_map(|b| match &b.editor {
            Some(markion_core::model::VisualBlockEditor::Table { cells }) => Some(cells.clone()),
            _ => None,
        })
        .unwrap();
    let target = cells
        .iter()
        .find(|c| c.row == 1 && c.column == 0)
        .unwrap()
        .field
        .source_range
        .start;
    harness.click(&mut editor, target);
    type_text(&mut harness, &mut editor, "filled");
    assert!(editor.text().contains("filled"));
    assert!(editor.text().contains("# literal"));
    assert_eq!(
        editor
            .state
            .document
            .visual_editor_field_at(&editor.selection())
            .unwrap()
            .kind,
        markion_core::model::VisualEditorFieldKind::TableCell { row: 1, column: 0 }
    );
}

#[test]
fn hidden_boundary_affinity_and_double_click_keep_marker_ownership() {
    let mut editor = Editor::new("**bold** plain");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    harness.click(&mut editor, 6); // immediately before closing **
    type_text(&mut harness, &mut editor, "X");
    assert_eq!(editor.text(), "**boldX** plain");
    editor.undo();
    let point = harness.point(&editor, 4);
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Some(point),
    );
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        Some(point),
    );
    harness.event(
        &mut editor,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Some(point),
    );
    assert_eq!(editor.selection(), 2..6);
    assert_eq!(&editor.text()[editor.selection()], "bold");
}

#[test]
fn source_toggle_invalidates_old_native_geometry_even_at_same_document_version() {
    let mut editor = Editor::new("**bold**");
    let version = editor.state.document.version();
    let revision = editor.surface_revision;
    editor.update(Message::ToggleSource);
    let selection = editor.selection();
    editor.update(Message::Hit {
        version,
        revision,
        display: 4,
        extend: false,
        word: false,
        affinity: VisualCaretAffinity::Downstream,
    });
    assert_eq!(editor.selection(), selection);
}

#[test]
fn image_handles_are_local_or_app_supplied_and_payload_source_is_elided() {
    use cosmic::iced::core::image::Handle;
    let mut editor = Editor::new("![alt](picture.png)");
    assert!(editor.image_handle("picture.png").is_none());
    editor.set_image_directory("/notes/assets");
    assert!(
        matches!(editor.image_handle("picture.png"), Some(Handle::Path(_, path)) if path == std::path::Path::new("/notes/assets/picture.png"))
    );
    assert!(
        editor
            .image_handle("https://example.org/image.png")
            .is_none()
    );
    editor.set_image_handle(
        "https://example.org/image.png",
        Handle::from_rgba(1, 1, vec![255u8; 4]),
    );
    assert!(
        editor
            .image_handle("https://example.org/image.png")
            .is_some()
    );
    let source = format!("![large](data:image/png;base64,{})", "A".repeat(4096));
    let mut editor = Editor::new(&source);
    editor.update(Message::ToggleSource);
    assert!(editor.surface.projection.text.len() < 200);
    assert_eq!(editor.text(), source);
    let token = editor
        .surface
        .projection
        .segments
        .iter()
        .find(|s| s.atomic)
        .unwrap()
        .clone();
    select(&mut editor, token.source_range.end, token.source_range.end);
    editor.update(Message::Action(Action::Edit(Edit::Backspace)));
    assert!(!editor.text().contains(&"A".repeat(100)));
    editor.undo();
    assert_eq!(editor.text(), source);
}

#[test]
fn native_edit_sequences_keep_utf8_mappings_and_checked_mutations_valid() {
    let mut editor = Editor::new("# 中\n\n**bold** and &amp; e\u{301} 👩\u{200d}💻\n\n- item\n");
    let mut harness = Harness::new(&editor, 270.0, 150.0);
    harness.click(&mut editor, 2);
    let mut random = 19u32;
    for _ in 0..120 {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        match random % 10 {
            0 => named(&mut harness, &mut editor, Named::ArrowLeft),
            1 => named(&mut harness, &mut editor, Named::ArrowRight),
            2 => named(&mut harness, &mut editor, Named::ArrowUp),
            3 => named(&mut harness, &mut editor, Named::ArrowDown),
            4 => named(&mut harness, &mut editor, Named::Backspace),
            5 => named(&mut harness, &mut editor, Named::Delete),
            6 => editor.undo(),
            7 => editor.redo(),
            8 => type_text(&mut harness, &mut editor, "é"),
            _ => named(&mut harness, &mut editor, Named::Enter),
        }
        assert!(editor.text().get(editor.selection()).is_some());
        for segment in &editor.surface.projection.segments {
            assert!(
                editor.text().get(segment.source_range.clone()).is_some(),
                "{segment:?}"
            );
            assert!(
                editor
                    .surface
                    .projection
                    .text
                    .get(segment.display_range.clone())
                    .is_some()
            );
        }
        assert!(editor.last_rejection().is_none());
    }
    assert!(
        editor
            .state
            .document
            .mutation_journal()
            .iter()
            .all(|entry| entry.rejection.is_none())
    );
}

#[test]
fn home_and_end_use_current_native_visual_line_not_another_block() {
    let mut editor = Editor::new("first paragraph\n\nsecond paragraph\n\nthird paragraph");
    let mut harness = Harness::new(&editor, 640.0, 320.0);
    let start = editor.text().find("second").unwrap();
    harness.click(&mut editor, start + 4);
    named(&mut harness, &mut editor, Named::Home);
    assert_eq!(editor.caret(), start);
    named(&mut harness, &mut editor, Named::End);
    assert_eq!(editor.caret(), start + "second paragraph".len());
}
