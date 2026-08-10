# Todo

Working mode: I (the agent) read this file, plan, implement, verify (`just lint` + tests + smoke), and move finished items to *Completed*. Drop issues/requests here anytime — I'll work through them, using parallel sub-agents where tasks touch disjoint files. create new tasks to become like futo notes. while being a cosmic app vibe. check the custome markdown editor component and its competness with existing editors. add improvable features as todo. and iterate. multiple agents are working parrallely so be cooperative.

> Parallel agent remark (2026-08-10): space-toggle is **implemented in the working tree** — unified on your `editor::space_toggles_todo` in `src/editor/mod.rs`; the `app.rs` binding arm now calls it (my earlier duplicate `AppModel::todo_space_toggle` + app.rs test were removed). Editor-completeness audit items added to Backlog below. Uncommitted, not pushed — include with your next commit.

*In Progress*

## Item 1 — Sidebar with collapsible directory tree + tabs (Futo-style layout)

Original request: like cosmic-player shows opened folder contents in the side panel — show notes as files/folders inside the cosmic-notes folder in a collapsible directory-tree sidebar. One sidebar on the left, growing editor/viewing panel on the right like Futo Notes. Right panel supports tabs for multiple open notes (pattern from cosmic-terminal / cosmic-files).

- [ ] 1.1 `src/notes/mod.rs`: add `Tree`/`TreeNode` model — recursive vault scan, folders-before-files, hidden/non-markdown excluded (same rules as `notes()`), rebuilt on vault mutations, unit tests
- [ ] 1.2 `src/sidebar.rs` (new): collapsible tree widget — chevron icons (`go-next`/`go-down`), indent by depth, click file → open note, click folder → toggle, selected highlight. Clean-room port of the cosmic-player *behavior* (GPL — imitate, don't copy); libcosmic MPL widgets only
- [ ] 1.3 Tabs: multiple open notes via libcosmic `tab_bar` (`segmented_button::Model`, closable, per-tab `data = PathBuf`). Switching tabs saves + reloads (per-tab editor state later)
- [ ] 1.4 Layout + state in `src/app.rs`: sidebar | divider | (tab bar + editor pane). Keep search + All/Pinned/Trash (segmented control in the sidebar); pinned shown with star; Trash as a collapsible section at the bottom. New messages: ToggleFolder / OpenNote / CloseTab / ActivateTab
- [ ] 1.5 i18n keys in `i18n/en/cosmic_notes.ftl`
- [ ] 1.6 Verify: `just lint`, `cargo test --all-features`, headless smoke; commit + push on `block-editor`

*Backlog* (not yet scheduled)
- [ ] Editor: drag-reorder block handles (`reorderable_flex_row` + `dnd_source`/`dnd_destination`)
- [x] Editor: space on `[ ]`/`[x]` marker toggles todo (done by parallel agent — working tree, uncommitted)
- [ ] Editor: multi-block select/operate (Shift+arrows, cut/copy/delete)
- [ ] Editor: `[[backlinks]]` across the vault
- [ ] Editor: images/attachments
- [ ] Tabs: per-tab editor state (blocks + undo/redo per open note, no save/reload on switch)

*Editor completeness audit* (parallel agent, 2026-08-10 — gaps vs Futo/Notion/Obsidian; unscheduled)
- [ ] Editor: find & replace in the active note (Ctrl+F panel: match across blocks, Enter next/prev, replace one/all)
- [ ] Editor: table block kind (GFM pipe tables: parse/serialize + in-editor grid, Tab moves cells)
- [ ] Editor: callout block kind (`> [!NOTE]`-style: icon + tinted background, collapsible)
- [ ] Editor: `[[` wiki-link autocomplete (picker of vault note titles, inserts `[[title]]`; pairs with backlinks item)
- [ ] Editor: code-block language tag → click to switch language; per-language highlighting via `text_editor::highlight`
- [ ] Editor: auto-pair `"` `'` `(` `[` `` ` `` while typing (unless a selection or following char makes it awkward)
- [ ] Editor: slash menu filters as you type (fuzzy match over the 10 entries, ↑/↓/Enter/Esc unchanged)
- [ ] Editor: duplicate block (Ctrl+D — inserts a copy of the block below, with undo)
- [ ] Editor: word/char count in a status bar (updates on edit, i18n keys)
- [ ] Editor: outline — collapsible heading list to jump to sections (Futo-style, lives in the sidebar)

*Pending*
- (new requests land here)

*Completed*
- [x] Block editor stages 0–4: typed-markdown conversion, Enter split/continue/exit, Backspace merge/un-nest, Tab indent, checkbox toggles, cross-block caret, toolbar + inline markers
- [x] Document-wide undo/redo (Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, 800 ms coalescing, 100-step cap)
- [x] Multi-line paste splits into blocks (markdown-aware: paste a list → list blocks)
- [x] Ordered-list renumbering on structural changes
- [x] Slash-command menu (`/` in empty paragraph; ↑/↓/Enter/Esc; 10 block kinds)
