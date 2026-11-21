# Release 0.72.0

Follow-ups from the 0.71.0 UI/UX audit: the remaining hard-coded font sizes in
the chat, sidebar, composer and chat-switcher chrome now scale with the
text-size preference, the rename inputs everywhere are caret-stable, and the
chat switcher shows unread badges. Source-level only; no render was taken in
the build container.

## Text-size preference reaches the rest of the chat chrome

23 hard-coded `font-size: Npx` rules in `app.css` covering the chat feed,
sidebar, composer, day separators, code blocks and the chat switcher are now
on the `--fs-*` tokens (`--fs-micro` for ≤11.5px, `--fs-caption` for
12–13px, `--fs-title` for message headings). The Tasks/Kanban/Calendar pages
still have their own px sizes — out of this audit's chat/Usage scope.

## Caret-stable rename inputs

The sidebar thread rename (`ui/shell.rs`) and the All-chats rename
(`ui/conversations.rs`) used a controlled `value:` binding, so every keystroke
re-rendered the row and reset the caret. Like the chat switcher in 0.71, they
are now uncontrolled: the initial text is set once on mount.

## Chat switcher: unread badges

Each chat row in the header switcher now shows the same unread badge as the
sidebar when the chat has unread messages.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy -p serabot-desktop --all-targets
  --locked -- -D warnings`, `cargo check -p serabot-desktop --features
  ui-check --locked`, `cargo test -p serabot-desktop --locked` (see CI).
- Not verified on a device: owner visual pass on Windows remains open.
