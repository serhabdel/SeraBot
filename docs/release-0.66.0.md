# Release 0.66.0

Route chosen model traffic through your own PC, and a quieter WhatsApp
presence. Tests use synthetic fixtures and a loopback end-to-end tunnel test.
Nothing here ran against a live provider, a real desktop, or WhatsApp.

## Route model traffic through this PC

Find it under **Settings → Local folders → "Route model traffic through
this PC"**. Turn it on and pick providers: Google Antigravity, the Gemini
API, Anthropic or OpenAI.

- **What happens:** while the desktop app is open, those providers' requests
  leave from your PC's internet connection instead of the server's. This helps
  where a provider refuses or limits data-centre addresses.
- **What your PC sees:** the connection stays encrypted from the server to the
  provider, so your PC only passes bytes along. It never sees keys, prompts or
  replies.
- **When the desktop is off:** the tunnel is refused at once and the bot's
  route chain fails over to its next route.
- **Limits:** only the fixed provider hosts on port 443. Both the server and
  your PC check the host list, and your PC refuses any private address after
  its own DNS lookup. Each tunnel is audited (host, bytes, duration; never
  content). The card shows "Relaying now" and has a "Stop relaying" button.
- **How it is built:**
  - Server: `relay_net.rs` (a loopback CONNECT listener with a per-member
    token), `/v1/relay/net` and `/v1/relay/net/exchange`, migration 0043, and
    a proxy hook when provider clients are built.
  - Desktop: `net_relay.rs` (long-poll threads over the existing API
    connection).
- The provider's terms still apply to how you use your account.

## WhatsApp: idle presence

- **Idle:** the linked device stays connected but shows "offline" when
  nothing is happening.
- **On a new message:** it shows "online" and "typing…" right away while the
  bot works, then goes back to "offline" after two idle minutes. New messages
  still start the bot immediately.
- `[whatsapp_link] presence_idle_secs` (default 120; 0 = always online).
