# SeraBot 0.37.0: marketplace plugin runtime

This release covers the plugin marketplace server (PR #70, merged), the desktop
Marketplace screen (PR #71, open and not yet merged), and the runtime that lets a
user's bots call the tools of plugins they have enabled (PR-L).

## Plugin marketplace server (PR #70)

- Catalog of ten plugins, one `plugins/<slug>/plugin.json` each. Each manifest
  declares its server, its sign-in fields, and the hosts it may reach.
- Per-user installs. A plugin is installed, then enabled, separately for each
  user. Uninstalling removes the install row.
- Keys are write-only. They are stored in the per-user sealed store under
  `marketplace.<slug>.<key>` and are never returned by the API.

## Plugin runtime (PR-L)

- **Tools.** An installed and enabled `http` plugin exposes the tools its MCP
  server lists. Each is named `plugin.<slug>.<tool>`, so it never collides with a
  built-in tool or with another plugin.
- **Per user.** The install, the key and the audit row all belong to the caller.
  Plugin tools are not added to the shared registry. They are read for each turn,
  so a plugin disabled or uninstalled drops out on the next turn.
- **Approval.** Every plugin tool call needs owner approval. Standing grants and
  auto-approval do not apply to plugin tools.
- **Egress.** Each request is checked first: https only, no credentials, the
  host must be on the plugin's list, and every resolved address must be public.
  The address rule is the one watcher fetches use.
- **Limits.** 30-second timeout per request, a 2 MB response cap, and at most
  8,000 characters of tool output returned to the model.
- **Audit.** Each call writes a `plugin.call` row with the tool name and whether
  it succeeded. Arguments and results are not written.

## Versions

- Server and desktop workspace: 0.37.0 (`Cargo.toml`, `Cargo.lock`).

## Not yet verified

- A live plugin call against a real provider. Tests use the shipped manifests and
  stop before any request leaves the process.
- The success path (MCP `tools/list` and `tools/call` answered by a server). It
  needs an https test server, which the test setup does not provide.
- DNS rebinding: the address is checked before the request, but the connection
  is not pinned to that address. This is the same limit as watcher fetches.
- Desktop Marketplace screen (#71): open, not merged, not in this build.
