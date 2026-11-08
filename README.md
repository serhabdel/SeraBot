<p align="center">
  <img src="crates/serabot/assets/brand/serabot-logo-512.webp" alt="SeraBot" width="160">
</p>

<h1 align="center">SeraBot</h1>

<p align="center">A self-hosted team of AI bots for a small, trusted circle.<br>
Deterministic Rust around bounded LLM calls.</p>

---

## What it is

SeraBot runs on your own server. Each person signs in with Microsoft and gets
their own private space: chats, memory, documents, bots, AI keys, connected
accounts and a vault of passwords and keys. Nobody can see anyone else's data.

| Part | What it does |
| --- | --- |
| **Server** (`crates/serabot`) | Bots, tools, approvals, audit log, SQLite store, Owner API |
| **Desktop app** (`crates/serabot-desktop`) | Windows app: chats, bots, live browser view, vault |
| **Mobile app** (`apps/serabot_mobile`) | Android app: chat, approvals, fleet |
| **Telegram** | Your own Telegram chat with your bots |

## Highlights

- **Bots that work together.** A main bot hands tasks to specialists
  (analyst, maker, watcher, accountant, auditor, marketer, markets, courier,
  inbox, WhatsApp) and you can create your own, with a picture, a model and a
  short interview that sets their role.
- **A real browser on the server.** Bots open public websites, read, click
  and type. You watch live and can take control at any time.
- **Vault.** Store logins and API keys once. Bots use them by name and never
  see the value.
- **Connectors.** OneDrive, Outlook, Outlook Calendar, Notion, and Google
  Drive, Sheets, Calendar and Gmail, per user.
- **Documents.** Build and edit Word, Excel, PowerPoint and PDF files, with
  cited answers from your own documents.
- **Safe by default.** Every tool call goes through one gateway with policy
  scanning and an audit log. Only destructive actions (deleting, sending)
  ask for your approval.

## Build

Windows (MSVC):

```bat
scripts\cargo-win.bat check --workspace --all-targets
scripts\cargo-win.bat test --workspace --locked
```

Linux:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
```

## Releases

Bump the workspace version in `Cargo.toml` on `main`. CI tags the release,
builds the desktop and mobile apps, and deploys the server. See
[docs/deploy-automation.md](docs/deploy-automation.md).

## Documentation

Start at [docs/README.md](docs/README.md). Rules for contributors and agents
are in [AGENTS.md](AGENTS.md).

All examples in this repository are synthetic. Never commit real credentials,
private documents, or brand-private material.
