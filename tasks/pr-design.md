Design pass on the desktop console, benchmarked against the Grok chat UI and following an Apple-HIG review (WWDC *Designing Fluid Interfaces*, *Principles of Great Design*) produced with DeepSeek V4 as a second pair of eyes.

The through-line: **the console already had good tokens; it talked about itself too much.** Every fix is "make the machine quiet, make the conversation loud."

## What changed

**Nominal state is now silent — only exceptions speak:**
- Sidebar `Connected` chip and topbar `Online` chip render **only when offline**. A permanent green chip trains the eye to ignore the one moment it matters.
- Bot liveness dots render only for the exception (`restart required`), not as a permanent green dot on every bot.
- The empty `Archived 0` sidebar section no longer renders.
- The task banner shows only **actionable** states (awaiting approval, failed) and never renders the task UUID — `Done main 01a196ab…` is gone from the reading path. The UUID stays available in the Inspector.

**The chat reads as a conversation, not a table:**
- Messages group by speaker: a speaker change opens a 16 px block; consecutive messages sit 2 px apart. Row hover highlight removed.
- Message text capped at **66ch**, 15 px / 1.55, with real heading rhythm inside assistant messages.
- Own messages are a speech bubble (`18px 18px 4px 18px`, no border) and drop the redundant `You` label.
- Timestamps are short: `10:33` today, `Oct 30 14:05` otherwise. Date math is dependency-free (Hinnant civil-from-days), covered by unit tests.
- The composer team row is **collapsed by default** into one caption chip (`Talking to N ▾`); the member pills only render expanded.

**Materials:** one light source at the top edge instead of three coloured blobs; cards and the inspector are opaque planes; `backdrop-filter` is reserved for chrome that actually moves over content (topbar, composer, menus, toasts). A disabled primary button loses its fill rather than ghosting at 50%.

## Deliberately not changed

- `--label-3` contrast: the review suggested a quieter tertiary colour, but `#7f7f89` on `--surface` drops below the documented 4.5:1 WCAG AA, so the token stays.
- Bigger structural passes (sidebar taxonomy merge, animated inspector drawer, suggested-reply chips) are documented in the review for a follow-up.

## Verification

- Windows: `fmt` clean, `clippy -D warnings` clean, **606 server + 125 desktop tests passing** (new tests cover the date helpers and the speaker-grouping class logic).
- `cargo check -p serabot-desktop --features ui-check` clean (type-checks the Dioxus rsx).
- CI will run the full matrix on this PR.
