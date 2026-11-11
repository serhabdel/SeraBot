# SeraBot 0.33.0: reliability, digest, and client polish

This release closes the v0.33.0 batch: server reliability and an on-demand digest
(PR #61), and desktop and Android polish (PR #62). Checklist: `tasks/big-wave.md`.

## Server reliability and digest (PR #61)

- **Retries with backoff.** Model, Telegram and connector calls retry short-lived
  failures (3 attempts, `Retry-After` honoured up to 30 s). Calls that may already
  have taken effect are never sent twice; only a refused request (429) or a failed
  connection is retried for those.
- **Own feeds show their URL.** A member sees the URL of feeds they added
  themselves. Feeds defined by the admin still show no URL to members.
- **Digest.** `GET /v1/digest` (`hours` up to 168, or `since`) lists new items and
  failing sources for the caller only. The same view is available in Telegram as
  `/digest [hours]`.

## Client polish (PR #62)

- **Source rows** show the feed host with a Copy button, or "URL unavailable"
  when no URL is shown.
- **Active plans** are labelled by chat name instead of an id.
- **Relative times** ("5m ago") for routine, source and plan updates.
- **Android:** message time and Copy on bubbles; long-press Rename and Archive in
  the chat list (with confirm and Undo); pull-to-refresh works on short lists.

## Not covered

- Manual device checks for the desktop and Android changes (see the PR checklists).
