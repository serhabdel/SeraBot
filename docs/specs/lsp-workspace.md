# Spec (proposed): LSP workspace on the desktop — rust-analyzer

Status: **proposed, not approved**. This widens the "one local-process
exception" (ACP, `docs/specs/claude-code-acp.md`) and therefore needs the
owner's explicit sign-off before any code. 0.74 ships the plain code workspace
(tree + editor + Problems click-through) without LSP.

## Goal

Make coding in a SeraBot project feel like a real IDE: live diagnostics on
save, hover, go-to-definition, and rename — via rust-analyzer over stdio, from
the Windows desktop only.

## Why rust-analyzer

- Single static binary, no Node runtime (the ACP exception covers Node for
  claude-agent-acp only; LSP must not need it).
- rust-analyzer is the reference Rust LSP: `publishDiagnostics`, hover,
  definition, references, rename.

## Security design (the part that needs approval)

1. **Desktop only.** The server never spawns LSP. (0.66's relay moves model
   bytes only; this is separate.)
2. **One allowlisted binary.** `rust-analyzer` resolved from PATH or an
   owner-configured absolute path set in Settings; no other server binary is
   ever started. Version pinned/logged at start.
3. **One allowed folder per project.** The LSP root is exactly the project
   folder the user opened in the workspace; rust-analyzer gets
   `--root-uri`/`workspaceFolders` limited to it. No folder outside the
   server's projects inbox is ever passed.
4. **Capability restriction.** The client sends only: initialize,
   textDocument/didOpen, didChange, didSave, hover, definition, references,
   rename, and subscribes to publishDiagnostics. It never sends
   workspace/executeCommand, codeAction, completion with snippet execution, or
   any workspace/* write. rust-analyzer's own config is locked
   (`initializationOptions` with `checkOnSave` enabled, no cargo extra args
   that touch the network beyond the local toolchain).
5. **No credentials in the child env.** Cleared environment except PATH,
   HOME/USERPROFILE, CARGO_HOME/RUSTUP_HOME (needed for the toolchain),
   SystemRoot; never the session token or provider keys.
6. **Bounded I/O.** Message size caps, idle timeout (kill after N minutes
   without requests), one LSP process per open project, killed on project
   close/window close. All traffic is local stdio; nothing is sent off-box.
7. **Edits stay approval-gated.** Rename results render as a diff preview;
   applying touches files through the same contained project-file write path
   (0.74 `PUT /v1/projects/{slug}/files/{*path}`), never directly from LSP.
8. **Audit.** LSP start/stop and rename-apply events go to the audit log.

## Phases

- 0.75: diagnostics-on-save via rust-analyzer feeding the existing Problems
  panel (server-stored diagnostics replaced by live ones), plus hover and
  go-to-definition in the workspace editor.
- 0.76: references + rename with diff preview and gated apply.

## Out of scope

Other languages' LSP servers (tsc, pyright, …) need Node/Python runtimes —
excluded by the product boundary unless separately approved. Multi-root
workspaces, remote LSP, LSP over the network relay: no.

## Verification plan

Synthetic LSP server fixture (a tiny stdio JSON-RPC fake) for client tests;
one live rust-analyzer run on the owner's Windows machine against a scratch
project before it is called working.
