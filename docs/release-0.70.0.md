# Release 0.70.0

A big one. Job finder that watches sites for you, a stronger browser, a daily
brief, email triage, memory review, research reports, bot health, real cost
tracking (cache prices, plans vs API), and a phone app that catches up.
Tests use synthetic fixtures. Nothing here ran against live job sites,
providers, mailboxes, OneDrive or a real browser.

## Costs, done properly

- **Cached tokens are counted apart.** Anthropic, OpenAI (Responses and
  chat), Gemini/Antigravity and DeepSeek-style cache hits are recorded per
  call. Older calls stay priced as plain input.
- **The price table has a cache column:**
  `provider model input cached output`, in USD per million tokens. Use `-`
  when there is no cache price.
- **Suggested prices:** "Load suggested prices" fills the editor with prices
  researched from public pages (`config/suggested_prices.json`). They are
  **not verified**, so check each line against the provider's pricing page
  before you save.
- **Totals in millions of tokens and $,** for this month and all time:
  input, cached and output, per model and in total.
- **Plans vs API:** calls made through a plan sign-in (ChatGPT/Codex,
  Copilot, Antigravity, Claude plan, Muse, OpenCode) show as "plan" instead
  of $0. Enter what each plan costs you per month to see what the same usage
  would have cost at API prices, and what you saved or lost.
- **API changes:** `GET /v1/usage/costs` now returns `totals`, `plans`,
  per-line `cached_tokens`, `plan` and `api_equivalent_usd`. New endpoints:
  `PUT /v1/usage/plans` and `GET /v1/admin/prices/suggested`.

## Job finder that watches for you

- **First chat interview:** the first time you talk to it, it asks a short
  set of questions: roles, cities or remote, sector, languages, level,
  keywords to include or avoid, how often to check, and your own tracking
  setup (a OneDrive Excel tracker, a materials folder, calendar deadlines).
  Everyone can set their own.
- **Site watching:** emploi-public.ma, alwadifa-maroc, anapec, rekrute,
  emploi.ma, dreamjob.ma, stagiaire.ma and marocannonces, plus any site you
  add.
  - Matches are checked against your profile and duplicates are dropped.
  - New matches reach you on Telegram or WhatsApp.
  - Public concours deadlines can become tasks.
- **Polite fetching:** scheduled checks follow each site's robots.txt. A
  site that disallows the page is only read when you ask.
- **Application pipeline:** saved, applied, interview, offer, rejected. The
  bot can also update your own OneDrive Excel tracker: it reads the file
  with the new read-only `onedrive_download`, edits it, and uploads it, and
  the upload asks for approval.
- **Desktop:** a "Job search" card on the Usage page.
- **Endpoints:** `/v1/jobs/*`. See `docs/jobs.md`.
- **Check the sources first:** the built-in listing pages and their parsers
  were not checked against the live sites. Check them on your server, and add
  corrected addresses as your own sources if needed.

## Browser upgrade (our Rust browser, not agent-browser)

agent-browser needs npm and its own Chrome download, which conflicts with the
no-Node rule. So its best ideas were brought into our own browser instead.

- **Safer clicks:** clicks and typing fail clearly when the target is hidden,
  disabled, read-only, gone, or covered (for example by a cookie banner).
- **Cookie banners:** dismissed automatically, preferring "reject
  non-essential". It never clicks payment, signup or login buttons.
  `[browser] auto_dismiss_banners = true` turns it on.
- **New actions:** `browser_find` (by role and name, or by text),
  `browser_wait`, `browser_select`, `browser_check`, `browser_scroll`,
  `browser_hover`, `browser_download`, `browser_pdf` and
  `browser_dismiss_banners`.
- **Approval:** `browser_upload` sends a file to a website, so it needs
  approval. `browser_forget_logins` wipes your saved logins, so it asks too.
- **Snapshot modes:** `interactive`, `full` and `text`. A light mode skips
  images and media (`[browser] block_heavy_resources`).
- **Logins:** each member's logins are kept between sessions.
- **Not yet tried in a real browser:** the in-page scripts (the click
  checks, banner detection, select/check/scroll, page text) and the real
  download and PDF flow. Check them on your server.

## Morning brief, email triage, memory review

- **Morning brief (daily, on by default):** deadlines and opportunities,
  today's calendar, tasks due, new job matches, new Outlook mail (count and
  3 subjects), cost against budget, and recent alerts. Button: "Send me my
  brief now". Set the time with `[ops] brief_hour_utc`.
- **Email triage (daily, off by default):** the inbox bot groups the last
  24 hours (important / needs reply / FYI / noise) and drafts replies.
  Sending stays approval-gated.
- **Memory review (weekly, on by default):** up to 10 memories that look
  contradicted, duplicated or stale. Keep, edit, merge or delete them on the
  Memory page.
- **Switches:** Usage → Alerts. Endpoints: `POST /v1/brief/preview`,
  `GET /v1/memory/review` and `POST /v1/memory/review/{id}`.

## Research reports and bot health

- **Research reports:** the researcher bot's deep-dive mode plans the
  questions, reads 10–20 sources, and builds a cited Word report with
  `research_report_build` (PDF optional). Every citation has to point to a
  source it read. Source URLs are plain text.
- **Bot health:** a card on the Usage page shows each bot's success rate,
  average and slowest reply time, top failing tool, and a "Needs attention"
  flag with the reason. Endpoint: `GET /v1/bots/health`.

## Phone app 0.70

- **Settings:** alerts and delivery, including the brief, triage and review
  switches and the digest and brief "now" buttons.
- **Costs:** cost per model, budget, totals in millions, and plans vs API.
- **Job search:** profile, sources, applications, and "Check now".
- **Not yet on the phone:** editing plan prices. Use the desktop for that.

## Migrations

| Migration | Adds |
| --- | --- |
| 0047 | plan prices, plus the cache price column |
| 0048 | job profiles, sources, seen jobs, watch state, applications |
| 0049 | memory reviews, plus the brief, triage and review switches |
