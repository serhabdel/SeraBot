# Claude Code on the person's computer, through ACP

Status: approved by the owner and shipped in 0.54.0
(`crates/serabot-desktop/src/acp/`, `ui_claude_code.rs`). It is a deliberate,
narrow exception to two `AGENTS.md` limits (no Node runtime, no local-process
runner without a security design). This document is that design.

## Why

A Claude plan sign-in only works inside Anthropic's own apps. SeraBot's server
calls the Messages API directly with the plan token, which is not one of those
apps, so the account ends up "Cooling down". Making the server pretend to be
Claude Code would get around Anthropic's terms; we will not do that.

The supported path is the one openbot uses: run the real Claude Code through
Anthropic's Agent SDK on the owner's own computer, signed in with the owner's
own `claude` login. `@agentclientprotocol/claude-agent-acp` wraps that SDK as
an Agent Client Protocol (ACP) agent: JSON-RPC over stdio, which the desktop
can drive natively.

## Shape

- **Where it runs:** the Windows desktop only, never the server. The server
  never sees the Claude token; Anthropic sees genuine Claude Code.
- **What it is:** a desktop **Claude Code** page (icon rail), not a provider
  for server bots. ACP is agent-level (Claude Code runs
  its own loop and tools), so it cannot be a raw model behind SeraBot's turn
  loop.
- **Prerequisites:** Node.js 22 or newer, installed by the person (the card
  offers `winget install OpenJS.NodeJS.LTS` in a console window). SeraBot does
  not bundle Node. The adapter ships Claude Code itself.
- **Sign-in:** Claude Code's own, run by the adapter in a console window
  (`npx -y <adapter> --cli auth login --claudeai`). The token stays in Claude
  Code's files; SeraBot never reads them. A session that answers ACP
  `auth_required` shows **Sign in to Claude** and **Try again**.
- **Launch:** one fixed command, `npx -y @agentclientprotocol/claude-agent-acp@0.100.0`,
  no shell, no model-chosen arguments, no window. The child gets an allowlist
  of environment variables (PATH, the profile folders, temp, proxy settings,
  `CLAUDE_CONFIG_DIR`); API keys and tokens in the desktop's environment are
  not passed. Stopping closes its input and ends the whole process tree
  (`taskkill /T`).
- **Workspace:** the person picks one of their allowed local folders
  (Settings, Local folders). ACP `cwd` is that folder. File requests the agent
  sends to the client (`fs/read_text_file`, `fs/write_text_file`) are answered
  only inside it, with the local-folder checks (no `..`, no links or
  junctions, no credential-like names, no program files, writes only in a
  read-write folder, 1 MiB reads, 256 KiB writes). Claude Code's own tools
  work in that folder under its default permission mode, which asks before
  edits and commands.
- **Permissions:** every ACP `session/request_permission` becomes a card with
  the agent's one-time choices and **Decline**. "Always" choices are never
  shown and are refused if sent back. Stop answers open requests "cancelled".
- **Terminal:** the ACP terminal capability is not offered.
- **Transcript:** streamed `session/update` chunks (text, thinking, tool
  calls, plans) render on the page. It lives in the desktop's memory only and
  ends with the app; mirroring it to the server would be a later, opt-in step
  that passes the blocklist like any outgoing text.
- **Who:** whoever is signed in to that desktop, on their own computer, with
  their own Claude login. The server neither offers nor sees it.

## Out of scope

- Running anything on the OVH server.
- Shipping Node or Claude Code with SeraBot.
- Letting a server bot call Claude Code as a tool (needs its own approval and
  egress design).

## Verification plan

Unit tests against a synthetic ACP agent (a fixture that speaks the protocol
over stdio). The owner runs it once against the real Claude Code before it
is called working.
