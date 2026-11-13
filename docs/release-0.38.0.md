# SeraBot 0.38.0: a steadier chat and one place for integrations

This release is mostly the desktop app, plus plugin tools on the server.

## Chat view and switching (#81)

- Bots you had not messaged yet now show their chats. The chat list used to be
  rejected whole when the server sent empty timestamps; it is now parsed
  tolerantly.
- A chat picker in the header, a visible New chat row in the sidebar, and
  Ctrl+K search over chat titles.
- The message box sits under the transcript instead of covering the last
  messages. Replies are plain text rather than boxes.

## Integrations (#82)

- Connectors, Composio and the Marketplace are one Integrations screen with
  Installed, Marketplace and Composio tabs, one search, and the same card and
  status pill everywhere. Secrets stay write-only.
- Links open from the app only when they are plain https with a host and no
  credentials.

## Plugin tools (#83)

- Marketplace plugins you turn on become tools your bots can use. Each plugin
  reaches only the hosts it declares, over https, with connections pinned to
  the addresses that passed the public-address check. Every call asks for
  approval, and plugin output is treated as untrusted data.

## Chat polish and safe links

- Links in bot replies open only if they are plain https, through the checked
  open path. Anything else (`javascript:`, `data:`, `file:`, links with
  credentials) shows as plain text.
- Images in replies show as a chip and never load by themselves.
- Long code blocks cap at a fixed height with a Show all button. Tables honour
  column alignment. Lists nest to three levels.
- Each chat keeps its scroll position, shows loading placeholders while its
  transcript arrives, and the header is tidier on narrow windows.
- Fixed a parser loop on table-like lines whose separator row has no `---`.

## Not covered by automated tests

Real-window rendering in WebView2, a live server, and plugin calls against a
real provider. Each PR carries a manual checklist.
