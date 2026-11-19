<!-- unverified: written from ratatui 0.29 public docs. Confirm against the pinned version. -->
# ratatui map (Rust TUI)

Immediate-mode drawing into a cell buffer each frame; input via `crossterm` or `termion`. Conventions in `platforms/tui.md`.

| Concept | ratatui |
| --- | --- |
| Component | a struct with `fn render(&self, area: Rect, buf: &mut Buffer)` (`Widget` / `StatefulWidget`), composed in `terminal.draw(\|frame\| ..)` |
| Layout | `Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Percentage(30)]).split(area)`; `Rect` math; `Flex` |
| Spacing | `Block::padding(Padding::uniform(1))`, `Layout::spacing(1)`, margins on `Layout` |
| Surface | `Block::bordered().border_type(BorderType::Rounded).title("..")`; borders are structure, use sparingly |
| Typography | `Style::default().fg(Color::..).add_modifier(Modifier::BOLD \| DIM \| ITALIC)`; `Span`, `Line`, `Text`; no sizes |
| Truncation | `Paragraph::wrap(Wrap { trim: true })` or manual truncation with `…`; `Line::alignment` |
| Lists, tables | `List` with `ListState` (selection), `Table` with `TableState`, `Row`, column `Constraint`s, numbers right-aligned via `Cell` alignment |
| Focus | an app-level `enum Focus` deciding which pane gets a highlighted border or `highlight_style`; Tab cycles |
| Keyboard | `event::read()` → `KeyEvent { code: KeyCode::Char('q'), modifiers }`; every action has a key shown in a help footer |
| Color fallback | semantic `Style`s defined once with 16-color fallbacks; never color alone, add a glyph |
| Feedback | spinner via `throbber-widgets-tui` or a tick counter; status line `Paragraph` at the bottom; `Gauge` for progress |
| Modals | `Clear` widget over a centered `Rect`, Esc closes |
| Scroll | `ScrollbarState` + `Scrollbar`, keep the selected row visible |
| Motion | none beyond spinners; redraw on events or a 60–250ms tick |
| Verification | `cargo check`; `TestBackend` snapshots (`Terminal::new(TestBackend::new(80, 24))`, `insta`) |

Web cheatsheet: focus ring → highlighted border or `highlight_style`; hit area → none, keyboard only; accessible name → the visible label is the name; reduced motion → not applicable.
