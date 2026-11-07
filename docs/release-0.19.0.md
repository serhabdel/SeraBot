# SeraBot 0.19.0

## Bots and tool access
- Existing users' bots now upgrade on their own. On first load, each user's fleet gains the tools, skills, delegates and new bots that shipped since their bots were seeded. Prompts are replaced only if the owner never edited them. Tools, bots or delegates the owner removed stay removed after the first upgrade.
- Main knows about `local_list`, `local_read` and `local_write` (folders on your PC that you allow in Settings > Local folders). If the desktop app is offline or a folder isn't allowed, the bot says so and how to fix it instead of claiming it can't.
- Analyst and Maker get read-only access to allowed PC folders.
- Team and handoff turns carry the same tool-honesty rule and skill list as direct chats.
- Prompts no longer name tools or skills their bot doesn't have (enforced by a test).

## New bots and avatars
- Seven new specialists: Comptable (Moroccan accounting and tax), Auditor, Marketer, Markets (research only, no trades or personal advice), Courier (Telegram), WhatsApp and Inbox (email). Messaging bots never send, reply or forward without approval.
- Every bot has a default mascot avatar. Uploaded pictures still win; removing one restores the default.
- 14 new built-in skills, including `ma-tax-2026`, `pcm-cgnc`, `ma-payroll`, `financial-analysis`, `audit-checklist`, `bank-reconciliation`, `campaign-plan`, `equity-research`, `email-triage` and `contract-review`. Figures marked "à vérifier" must be checked against the current Bulletin Officiel.
- `docs/knowledge-packs.md` explains how to index your own reference documents for a bot.

## Memory
- Recall ranks every saved fact by relevance, importance and recency. Before, only the 20 newest facts were used.
- After each owner turn, durable facts are extracted automatically. Facts from turns that read web pages, documents or files wait under "To review" in Memory until you approve them. Settings: `[memory] auto_extract`, `extract_model`, `extract_min_interval_secs`.
- Long conversations get a rolling summary that is used in that conversation only.
- New tools: `memory_search` and `memory_update`. Bots refuse to store credential-like text in memory and point you to the Vault.
- Optional Mem0 recall source (`[memory] provider = "mem0"`, key in your Vault as `conn.mem0`). Off by default.
- The Memory page has search, filters (Saved, Learned, To review, Superseded), inline edit and stats.

## Composio
- Errors show what Composio actually said, with a fix: rejected key, plan limit, not found, rate limit, unavailable. A Composio error no longer appears as "invalid request" or "HTTP 500".
- If Composio rejects a key as a project key, it is retried once as a user key.
- Reply, forward, share, invite and post actions now need approval.
- `[composio] tool_version` pins a toolkit version; the default is `latest`.
- Rolling back to 0.18 after connecting a key means entering the key again.

## Chat
- Drag files onto the chat or paste images into the composer. Image attachments show thumbnails and open full size.
- Stop a running reply. In-flight tools finish, the remaining steps are dropped, and nothing is replayed.
- Each chat keeps its own draft.
- Message actions: copy, edit last message, regenerate last reply.
- Code blocks have a language label, a copy button and syntax highlighting.
- Day separators, a scroll-to-latest button with an unread count, a clearer "is working" line, and suggestions in empty chats.
- Translucent composer that turns solid with reduced transparency or reduced motion.

## Not verified
- The new screens have not been viewed in the running desktop app. Drag-drop and paste have not been tried in WebView2.
- Composio, Mem0 and model calls were tested against mocks only, not live accounts.
- Memory recall matches words, not meaning: a paraphrase with no shared words isn't found.
- Images in bot replies don't get thumbnails yet.
