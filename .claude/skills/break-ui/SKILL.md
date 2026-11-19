---
name: break-ui
description: 'Try to break a UI component or screen by feeding it realistic worst-case data (long names, unbreakable emails and IDs, one-letter names, missing fields, zero items, exactly one item, huge counts, long labels, non-Latin and RTL text, emoji, extreme numbers, slow and failed loads), render both datasets behind a demo/worst-case toggle or test fixture, and report what broke. Works for Rust desktop (Freya, egui, gpui, Slint, Iced), web and mobile. Use when the user asks to stress test, find edge cases, check how a design holds with real data, or before a release. Usually invoked by the definitive-ux router; can be called directly.'
---

# Break UI

> **Stack:** load `stack-map` first for the toolkit's test and fixture idioms. Adapted from Emil Kowalski's `break-ui` skill.

An adversarial skill. Demo data is chosen, usually without noticing, to make the design look good: names that fit on one line, counts that never need a separator, every optional field filled. The job is to undo that choice one field at a time, show both datasets side by side, and report what broke. It does not redesign (`design-review`, `better-interface`) or fix (the user decides).

## Posture

You are the most annoying real user this component will ever meet. Your name is Aleksandra Wiśniewska-Kowalczyk, your colleague's email is `bartholomew.fitzgerald@northwind-industries-holdings.example.com`, your intern is called Jo, your workspace has 1,204,882 items, your language is Arabic today and Japanese tomorrow, your network is a train.

Two failure modes, the first worse:

1. Nonsense data. Five thousand `a`s prove nothing; the designer rightly says "that never happens". Every worst case must be something a real user could produce.
2. Stopping at long text. The ones that ship are the short name that leaves an orphaned dash, the missing avatar, the count of exactly 1 ("1 members"), the empty list, the badge whose text got translated 40% longer, the timestamp at midnight on New Year's Eve.

## Workflow

1. **Inventory every value the UI renders**: strings, numbers, dates, images, lists, optional fields, async results. Use the component's props or state struct as the checklist.
2. **Build the worst case for each**, realistic and specific:
   - Strings: 1 character, 60+ characters with no spaces (an email, a URL, a hash), a 200-character sentence, RTL (Arabic), CJK, emoji, combining diacritics, leading and trailing spaces.
   - Numbers: 0, 1, negative, 999,999,999, 0.001, a currency with 3 decimals, a percentage over 100.
   - Dates: now, 1 minute ago, 11 months ago (the "1 year" rounding), a year 2099, timezone edges, 24-hour vs 12-hour.
   - Lists: empty, exactly one, two, 10,000 (virtualization), all items identical, items of wildly different heights.
   - Optional fields: every combination of missing.
   - Images: missing, slow, 1×1, 4000×100 panorama, transparent, portrait in a landscape slot.
   - Async: pending for 5 seconds, failed, succeeded with partial data, succeeded then revoked.
   - Window: minimum size, 200% text scale, a narrow dock beside a pane.
3. **Render both datasets behind a toggle.** Freya: a `use_state` bool and two fixture constants, or a `freya-testing` test that renders both and snapshots each. egui: a `ui.checkbox` in a debug panel switching the fixture, or `egui_kittest` snapshots. gpui: a test with `TestAppContext` or a dev flag. Slint: a second preview with `--style` and a data model fixture. Web: a `?worst=1` query or a Storybook story. Keep the toggle out of production builds.
4. **Report** what broke, by field, with a screenshot or snapshot path when available.

## Report

| Field | Worst case | What happened | Severity |
| --- | --- | --- | --- |
| `user.name` | `Aleksandra Wiśniewska-Kowalczyk` | overflows the row, pushes the action button off | Blocking |
| `members.len()` | 1 | "1 members" | Polish |

Severity: Blocking (content unreachable, action pushed off, crash, data hidden), Important (clipped without ellipsis, layout jump, wrong plural, unreadable), Polish (orphan, awkward wrap).

End with the three fixes that remove the most rows (usually: truncate with the full value reachable, min/max widths from the type scale, pluralization), and leave the fixing to the user or the review skills. Never delete the worst-case fixture; it is the regression test.
