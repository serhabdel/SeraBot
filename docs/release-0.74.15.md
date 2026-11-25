# Release `0.74.15` (in progress)

Plan: `docs/specs/0.74.15-proposal.md`. This file grows as pieces land; the version is bumped when the batch is cut.

## Landed

- **Mail composer**: sticky footer (Send is always visible), wider drawer, chip fields for To/Cc/Bcc (Enter, comma, semicolon, Tab, a space after a finished address, or a paste commits; Backspace removes the last chip; arrow keys pick suggestions; invalid addresses are marked and block Send), attach from this PC, by drag and drop, or from the Library (10 files, 18 MB in total), Ctrl+Enter sends.
- **Chat message bar**: Copy, Quote, Create task and Edit sit in a row under their message instead of floating over the neighbouring one.
- **Retried messages**: a message sent again with no reply in between shows once. Transcript only; the server still stores both copies. Real recall and retry-by-reference are proposed (Track 7b) and need approval.

## Owner verification

- Windows: drop files on the composer, press Enter in To, check the footer in a short window, check the chat bar.
