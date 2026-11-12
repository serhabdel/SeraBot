# Live step events (0.37)

The server streams one `turn.step` event per tool phase on `GET /v1/events`.
After a reload, `GET /v1/conversations/{id}/activity` returns the same events
for that conversation, oldest first (at most 200).

Existing fields stay. 0.37 adds `tool`, `input_summary`, and, when they
apply, `result_summary`, `error`, `duration_ms`, `cards`, and `plan`.
Clients that only read `phase`, `icon`, and `label` keep working.

## Phases

| `phase` | Meaning |
| --- | --- |
| `running` | The tool call started. |
| `progress` | The call returned and the work is still going (a generation, or a background media job). |
| `waiting_approval` | The owner must approve a destructive action. `approval_id` is set. |
| `done` | The call finished. |
| `failed` | The call failed, the owner denied it, or policy blocked the output. |

`running` is the started card. `done` is the finished card. `failed` is the
failed card.

## Payload

```json
{
  "task_id": "…",
  "turn_id": "…",
  "conversation_id": "dir:main",
  "bot": "legal",
  "call_id": "…",
  "step": 1,
  "phase": "done",
  "tool": "tavily_search",
  "icon": "search",
  "label": "Searching the web",
  "input_summary": "Searching the web",
  "result_summary": "Tavily search",
  "duration_ms": 840,
  "cards": [],
  "plan": {
    "goal": "Find the filing",
    "done": 1,
    "total": 3,
    "items": [{ "text": "Search", "status": "done" }]
  }
}
```

- `tool` is the tool name. `input_summary` is a short label built from the
  tool name and, at most, the host of a URL. Queries, file contents, and
  secrets are never copied into the event.
- `result_summary` is the first line of the tool text, at most 160
  characters, and only when it passes the content policy.
- `error` is a short message. If it looks like a credential, the event says
  `The step failed.` instead.
- `duration_ms` is how long the call took. A resumed approval has no duration.
- `cards` uses the schema in [Tool results](tool-results.md). Omitted when
  empty or when the policy rejects the card text.
- `plan` is the conversation's current goal and checklist. Omitted when there
  is no plan or the plan text is blocked.

A background media job emits another `turn.step` with `tool` `media_job`
and `call_id` equal to the job id when the file is saved or the job fails.
The older `media_job.created` and `media_job.updated` events are unchanged.

`help.queued` is emitted when `ask_teammate` queues a teammate. It carries
ids and bot names only.
