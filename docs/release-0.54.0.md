# Release 0.54.0

Claude Code on your own computer, inside SeraBot, with your own Claude login.

## Claude Code page

A new **Claude Code** page in the desktop (icon rail, under Chats) runs the
real Claude Code through the Agent Client Protocol
(`@agentclientprotocol/claude-agent-acp@0.100.0`). Anthropic accepts a Claude
plan only in its own apps; this is its own app, run by Anthropic's SDK, so the
plan works. SeraBot does not pretend to be Claude Code anywhere.

- **Setup card:** checks for Node.js 22 or newer (offers a `winget` install),
  lets you pick one of your allowed local folders, and opens Claude Code's own
  sign-in in a console window. Nothing else to install: the adapter brings
  Claude Code.
- **Chat:** streamed answers, collapsible thinking, tool calls that update in
  place, the current plan, a working indicator, Stop, and Ctrl+Enter to send.
- **Permission cards:** every edit or command Claude Code asks for waits for
  **Allow** or **Decline**. "Always" is never offered.
- **Folder fence:** file requests Claude Code sends to SeraBot are answered
  only inside the chosen folder, with the local-folder checks.
- **Keeps running** while you use other pages; **Stop Claude Code** ends the
  whole process tree.
- The server is not involved, and your Claude token never leaves Claude Code.

Design and limits: `docs/specs/claude-code-acp.md`.

## Model calls (finishing 0.53.1)

- Model calls time out on silence (the configured seconds), not on total
  time; a whole call is capped at 15 minutes. Long thinking answers through
  Antigravity were cut at 120 s and the chat spun across every Google host.
- A failed model call says so in the chat at once (status and failure type,
  never the provider's reply body), and whether the turn tries again. A turn
  waiting for a model says until when and the last error.
- No `anthropic-beta` header is sent to Cloud Code, as in HielCowork.

## Not verified

- The ACP client is tested against a synthetic agent over pipes; the page was
  type-checked and its stylesheet checked in a static render. It has not run
  against the real adapter or on a Windows device. Sign in and send one
  message before calling it working.
- No provider was used live.
