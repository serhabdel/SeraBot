# SeraBot skills

On-demand operator skills live here as markdown packs (`SKILL.md` per skill id).
Bots declare an allowlist in `bots/*.toml`; at runtime they call `skill_list`,
`skill_search`, and `skill_load` through the gateway.

Layout:

- `skills/<id>/SKILL.md` — bot workflow skills (triage, deck, legal-rag, …)
- `skills/packs/<pack>/<id>/SKILL.md` — shared packs (for example `office/`)
- `skills/skill-creator/SKILL.md` — how to add a new skill

Scaffold a new skill:

```bat
scripts\cargo-win.bat run -- skill init my-skill --description "Short summary"
```

Then add the id to the relevant bot's `skills = [...]` array and include
`skill_list`, `skill_search`, and `skill_load` in that bot's `tools` list.

Skill ids must match `^[a-z][a-z0-9-]{0,63}$`. Content is synthetic in the
public repository; never commit private dossier material into skills.
