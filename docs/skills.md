# On-demand skills

Skills are markdown instruction packs under `skills/` (see `skills/README.md`).
Bots declare an allowlist in `bots/*.toml`; models load instructions at runtime
via gateway tools—nothing is injected into the base prompt until `skill_load`
runs.

## Runtime tools

| Tool | Effect |
| --- | --- |
| `skill_list` | Summaries for skills allowlisted on the active bot |
| `skill_search` | Filter summaries by id/name/description |
| `skill_load` | Full `SKILL.md` body for one allowlisted id |

All three are non-external: no approval, no secrets. Output still passes egress
policy scanning like any tool result.

## Configuration

```toml
[skills]
dir = "skills"
```

Relative paths resolve against the directory that contains `serabot.toml`.

## Operator workflow

```bat
scripts\cargo-win.bat run -- skill init my-skill --description "Short summary"
scripts\cargo-win.bat run -- skill list
scripts\cargo-win.bat run -- check
```

After scaffolding, edit `skills/<id>/SKILL.md`, add the id to a bot's `skills`
array, and include the three skill tools in that bot's `tools` list.

## Packs

Shared deliverable skills live under `skills/packs/<pack>/<id>/SKILL.md` (for
example the `office` pack: `docx`, `xlsx`, `pptx`, `design`). Pack membership is
metadata only; bots still opt in by skill id.

Built-in integration skills (`firecrawl`, `onedrive`, `audio`, `elevenlabs`,
`scheduler`) document optional subsystems configured in `serabot.toml`.
