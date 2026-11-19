# Terminal UI (ratatui and friends)

A grid of cells, keyboard only in practice, 16 to 256 colors or truecolor depending on the terminal, no fonts of your own, mouse optional and never required.

## Conventions to check

- **Keyboard complete**: every action has a key, shown on screen in a persistent help line or footer; vim-style and arrow keys both work for navigation; `?` opens help; `q` or Esc leaves.
- **Focus visible** as a highlighted row or a bordered pane; one focused pane at a time; Tab cycles panes.
- **Layout from constraints**, not pixels: panes that resize gracefully down to 80×24 and stay usable; long text wraps or truncates with an ellipsis and a way to see the rest.
- **Color with a fallback**: semantic styles (error, warning, selected) that still read in 16 colors and in a light terminal theme; never color alone for state (add a glyph or text).
- **Density is the point**: tables with aligned columns, numbers right-aligned, a status bar, no decorative borders around everything.
- **Feedback**: a spinner or progress for anything over 200ms, a status line message for every action, errors inline where they happened.
- **Modals** are rare and dismissable with Esc; prefer a pane or an inline prompt.
- **Unicode**: box-drawing and symbols only where the terminal supports them, with ASCII fallbacks.

## Motion

None beyond spinners and progress; redraw only on events or at a low tick rate.
