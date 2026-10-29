---
name: OneDrive dossier
description: List or upload an existing artifact in the approved OneDrive folder
---

# OneDrive

Use this after a deliverable already exists as an artifact hash. OneDrive is not a general file browser.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

## Tools

| Tool | Effect | Arguments |
| --- | --- | --- |
| `onedrive_list` | Read-only listing of the configured dossier folder | optional `subfolder`, optional `max_pages` (1–10) |
| `onedrive_upload` | Upload one stored artifact; requires approval | `artifact_hash` (required), optional `subfolder` |

## Workflow

1. Call `onedrive_list` when the owner asks what is already in the folder.
2. Upload only a hash a previous tool returned (`deck_build` or another artifact tool). Pass that exact `artifact_hash`.
3. Wait for approval. The approval binds the destination path and the artifact hash. Changing either needs a new approval.
4. Report the path from the tool result. Do not claim the file is in OneDrive before that result.

## Do not

- Invent a hash, a web URL, or a folder outside the configured dossier.
- Upload inbox files that have not been stored as artifacts.
- Read or print secret-store values. The refresh token stays in the age store.

## Missing public facts

OneDrive does not answer public questions. If a fact is missing from the folder listing and loaded skills, use `firecrawl_search` or `firecrawl_scrape` only when those tools are allowlisted. Otherwise say what is missing.
