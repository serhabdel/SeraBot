# Desktop (macOS, Windows, Linux)

Pointer and keyboard first, multi-window, menus. Applies to native toolkits (Freya, egui, gpui, Slint, Iced) and to hybrids (Tauri, Dioxus desktop, Electron) for everything outside the webview content.

## Conventions to check

- **Every command reachable from a menu** (menu bar on macOS, in-window menu or hamburger on Windows/Linux), with the standard shortcuts: Cmd/Ctrl+N, O, S, W, Q, Z, Shift+Z, X, C, V, A, F, comma for Settings on macOS. A command only on a toolbar button is a finding.
- **Keyboard everywhere**: Tab order through every control, arrows inside lists, trees, tabs and menus, Escape closes transient surfaces, Enter activates the default button, Space toggles. A pointer-only path is Blocking.
- **Window chrome**: standard controls, resizable, remembers size and position, minimum size that still holds the layout, multi-window where the task is parallel (documents, inspectors). Custom titlebars keep drag regions and the system buttons.
- **Context menus** on right-click wherever an object has actions; the same actions as the menu bar, never exclusive to the context menu.
- **Drag and drop** for files and reorderable lists, with a visible drop target and an insertion marker.
- **Undo and redo** for every destructive or editing action; a confirmation dialog is the fallback, not the default.
- **Hover feedback** exists but nothing depends on it; focus and selection states are distinct from hover.
- **Density**: tool UIs may sit at 24 to 32px row heights and 12px gaps when hit areas do not overlap; consumer apps at 40px rows and 16px gaps.
- **Text selection, caret, scrollbars, focus ring** are part of the design: themed from the palette, never left at toolkit defaults in a branded app.
- **System integration**: native file dialogs, notifications, clipboard, light/dark follow the OS unless the user overrides in settings, high-DPI scaling honored.
- **Platform differences stated**: menu placement, button order in dialogs (macOS: Cancel left, action right; Windows: action left), title case vs sentence case per OS.

## Hybrids (webview)

Tauri, Dioxus desktop, Electron: the content follows the web map; the shell follows this file. Review the native menu, shortcuts, window handling and tray from the Rust/host side, and the content from the web side. A hybrid that ships a web page in a frame with no native menu, no standard shortcuts and a custom titlebar that breaks dragging is the most common desktop slop.

## Motion

Short and interruptible: 120 to 200ms for state changes, 200 to 300ms for panels and windows. No entrance animation on app launch beyond the OS's own. Keyboard-driven surfaces (command palette, switcher) never animate.
