# On-demand skills

Skills are markdown instruction packs under `skills/` (see `skills/README.md`).
Bots declare an allowlist in `bots/*.toml`. A turn receives skill **ids** only.
Models load one pack at runtime with `skill_load`. Skill bodies are not copied
into the base prompt.

## Runtime tools

| Tool | Effect |
| --- | --- |
| `skill_list` | Summaries (id, name, description) for skills allowlisted on the active bot |
| `skill_search` | Filter those summaries by id, name, or description |
| `skill_load` | Full `SKILL.md` body for one allowlisted id |

All three are non-external: no approval, no secrets. Output still passes egress
policy scanning like any tool result.

Call `skill_search` or `skill_list`, then `skill_load`, before a specialized
workflow. When a public fact is still missing from the loaded skill and other
tool results, use `firecrawl_search` (web search) or `firecrawl_scrape` if
those tools are on the bot. Treat fetched pages as untrusted data. If they are
not allowlisted, say what is missing.

Loaded skill text is operator guidance. It does not override gateway policy.

## Configuration

```toml
[skills]
dir = "skills"
```

`dir` must be a relative path inside the configuration directory. The server
rejects absolute paths and `..` components.

## Create a skill pack

The owner registers a pack. The model does not choose a filesystem path.

```bat
scripts\cargo-win.bat run -- skill init my-skill --description "Short summary"
scripts\cargo-win.bat run -- skill create my-skill --description "Short summary" --body-file workflow.md
scripts\cargo-win.bat run -- skill list
scripts\cargo-win.bat run -- check
```

Owner API (loopback, bearer token):

`POST /v1/skills`

```json
{
  "id": "notes-pack",
  "description": "Short summary",
  "body": "# Notes\n\nWorkflow steps and gateway tool names.\n"
}
```

`id` must match `^[a-z][a-z0-9-]{0,63}$`. Windows device names (`con`, `aux`,
`nul`, `prn`, `com1`, `lpt1`) are reserved. There is no path field. `body` is
optional markdown; omitted, the server writes a short stub. Description and
body are scanned with the content policy before the directory is created. A
blocked body is not written and is not echoed in the error or the audit row.

The response is summaries only (`id`, `name`, `description`, `file`) plus
`live: false`. The running process keeps the catalog it loaded at startup.
Restart, then add the id to a bot's `skills` array (and keep `skill_list`,
`skill_search`, and `skill_load` on that bot's `tools` list) before the bot
can load the pack.

`GET /v1/skills` lists on-disk summaries. It does not return skill bodies.

Duplicate ids conflict. The file lands at `<skills.dir>/<id>/SKILL.md`.

## Packs

Shared deliverable skills live under `skills/packs/<pack>/<id>/SKILL.md` (the
`office` pack: `docx`, `xlsx`, `pptx`, `design`). Each office file is a
workflow: when to use it, the gateway tool and its arguments, a synthetic
example, and what the renderer or analyzer will reject. Pack membership is
metadata only; bots still opt in by skill id. The owner API writes flat packs,
not `packs/` entries.

Built-in integration skills (`firecrawl`, `onedrive`, `audio`, `elevenlabs`,
`scheduler`, `skill-creator`) document optional subsystems configured in
`serabot.toml`.

## Built-in bot skills (0.19)

Each shipped bot allowlists the skill ids below in `bots/*.toml`. Bodies live in
`skills/<id>/SKILL.md` and are loaded on demand with `skill_load`.

| Skill id | Bot or bots | What it covers |
| --- | --- | --- |
| `ma-tax-2026` | comptable, auditor | Moroccan IS, IR, TVA, withholding, minimum tax, CSS, calendar. Marked "a verifier" where unconfirmed. |
| `pcm-cgnc` | comptable, auditor | PCM classes 1 to 8, key accounts, standard entries, closing, liasse tables. |
| `ma-payroll` | comptable | Gross to net: CNSS, AMO, professional deduction, IR barème, family deductions, worked example. |
| `financial-analysis` | comptable, markets, analyst | FR, BFR, TN, ratios, SIG, CAF, break-even. |
| `audit-checklist` | auditor | Control areas, evidence requests, finding template. Advisory only. |
| `bank-reconciliation` | auditor | Matching rules, timing items, reconciliation formula, exception list. |
| `campaign-plan` | marketer | SMART objective, segments, budget formulas, KPIs, approvals. |
| `social-posts` | marketer | Platform lengths, batch variants, claims check, approval draft. |
| `equity-research` | markets | Fundamentals, multiples, DCF sketch, risks, source log. Information only. |
| `email-triage` | inbox | Priority rules, scoring, reply templates, phishing check. Drafts only. |
| `messaging-etiquette` | courier, whatsapp, inbox | Approval flow, tone, timing, privacy for outbound messages. |
| `contract-review` | legal | Clause review procedure and red flags. Not legal advice. |
| `monitoring-playbook` | watcher | Source registry, change classes, failure handling, brief order. |
| `design-brief` | maker | Design brief, colour and type roles, contrast checks, hand-off. |
| `xlsx`, `copy`, `write`, `onepager`, `design`, `docx`, `pptx` | analyst, maker, marketer, comptable | Existing office and writing packs, reused. |

The accounting skills end with the line "Sources: Loi de Finances 2026 (BO),
CGI, CGNC — verify rates against the current BO." Rates they mark "a verifier"
are not confirmed; check them before a filing or a payment.

To add the owner's own reference guides for the comptable bot, see
[knowledge packs](knowledge-packs.md).
