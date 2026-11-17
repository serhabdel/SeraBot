# Claude Code on the owner's computer, through ACP (proposal)

Status: proposal for 0.54, waiting for the owner's approval. It is a deliberate
exception to two `AGENTS.md` limits (no Node runtime, no local-process
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
- **What it is:** a desktop "Claude Code (this computer)" agent in the chat
  picker, not a provider for server bots. ACP is agent-level (Claude Code runs
  its own loop and tools), so it cannot be a raw model behind SeraBot's turn
  loop.
- **Prerequisites the owner installs:** Node.js and a signed-in Claude Code
  (`claude` login). SeraBot bundles neither. The card checks both and says
  what is missing.
- **Launch:** one fixed command, `npx -y @agentclientprotocol/claude-agent-acp@<pinned>`,
  no shell, no model-chosen arguments. The child gets a cleared environment
  (PATH, HOME/USERPROFILE, APPDATA, the Claude config dir), runs in its own
  process group and is killed with the chat.
- **Workspace:** the owner picks one folder per chat (the existing local
  folders contract). ACP `cwd` is that folder; the client refuses `fs/*`
  requests outside it.
- **Permissions:** every ACP `session/request_permission` becomes a SeraBot
  approval card (allow once / deny). Nothing is auto-allowed; there is no
  "always" in the first version.
- **Terminal:** ACP terminal capability off in the first version; Claude Code
  then uses its own sandboxed tools, still behind the permission cards.
- **Transcript:** streamed `session/update` chunks (text, thinking, tool
  calls, plans) render with the existing chat cards. The chat is stored on the
  owner's computer; mirroring it to the server is a later, opt-in step and
  passes the blocklist like any outgoing text.
- **Who:** the owner's desktop only. Members use it on their own computers
  with their own Claude login, or not at all.

## Out of scope

- Running anything on the OVH server.
- Shipping Node or Claude Code with SeraBot.
- Letting a server bot call Claude Code as a tool (needs its own approval and
  egress design).

## Verification plan

Unit tests against a synthetic ACP agent (a fixture that speaks the protocol
over stdio). The owner runs it once against the real Claude Code before it
is called working.
