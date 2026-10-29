# SeraBot skills

On-demand operator skills live here as markdown packs (`SKILL.md` per skill id).
Bots declare an allowlist in `bots/*.toml`; at runtime they call `skill_list`,
`skill_search`, and `skill_load` through the gateway.

Layout:

- `skills/<id>/SKILL.md` — bot workflow skills (triage, deck, legal-rag, …)
- `skills/packs/<pack>/<id>/SKILL.md` — shared packs (for example `office/`: `docx`, `xlsx`, `pptx`, `design`)
- `skills/skill-creator/SKILL.md` — how to add a new skill

Each `SKILL.md` is an instruction body with a heading and a workflow. `skill_load` returns that body. A file that is only a placeholder is not a pack.

Register a new skill (the owner writes the file; bots load it later with
`skill_load`):

```bat
scripts\cargo-win.bat run -- skill init my-skill --description "Short summary"
scripts\cargo-win.bat run -- skill create my-skill --description "Short summary" --body-file workflow.md
```

Or `POST /v1/skills` (see `docs/skills.md`). Then add the id to the relevant
bot's `skills = [...]` array, restart, and include `skill_list`, `skill_search`,
and `skill_load` in that bot's `tools` list. Bodies stay out of the base prompt.

Skill ids must match `^[a-z][a-z0-9-]{0,63}$`. Content is synthetic in the
public repository; never commit private dossier material into skills.
