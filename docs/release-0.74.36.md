# Release 0.74.36 — Browser accounts & captcha reduction, faster live view

## Account connection & fewer captchas

- **Account sign-in status (new, read-only)**: `GET /v1/browser/sessions/{id}/accounts`
  (owner) and the bot tool **`browser_account_status`** report, per browser
  profile, which external accounts are signed in — Google, YouTube, LinkedIn,
  Instagram, X, Facebook — derived from cookie **names and domains only, never
  values** (`browser/accounts.rs`, UID-scoped, audited as `browser.accounts`).
  Bots can check before navigating instead of walking into a captcha on a
  logged-out profile.
- **Google added to the connect-in-browser flow**: the Social profile
  (`POST /v1/browser/social/open`) now lists Google alongside LinkedIn,
  Instagram, X, Facebook and YouTube Studio, so the owner can complete a
  Google sign-in once in the Browser tab. The refusal-page rule is unchanged:
  when Google answers `signin/rejected`, SeraBot surfaces guidance and never
  retries or bypasses.
- **Anti-automation-detection hardening**: `[browser] hide_automation`
  (default on) passes `--disable-blink-features=AutomationControlled` so
  `navigator.webdriver` reads undefined and sites are less likely to serve a
  captcha to the server browser in the first place.

## Faster live view over a slow link

- **Push frames server-side**: `GET /v1/browser/sessions/{id}/frames` streams
  live JPEG frames over SSE from a CDP `Page.startScreencast` (frames capped at
  512 KiB), so streaming clients no longer need one HTTPS round trip per frame.
- **Adaptive polling on the desktop**: while the bot drives, the live view now
  polls every 1200 ms at JPEG quality 50 instead of 500 ms at 70; while the
  owner holds control it stays at 220 ms / quality 70. Screenshots accept `?q=`
  (20..=90) to cap the starting quality. Over the Cloudflare tunnel this cuts
  live-view traffic roughly 4–6× while the bot is driving.

## Verification note

- Synthetic unit tests: account-status mapping (no cookie values leak),
  launch flags, screencast frame handling, screenshot quality cap, desktop
  cadence/quality selection. Run with `cargo nextest run` /
  `scripts/test-changed.*`. Live Google sign-in was not exercised; the
  refusal-page path is covered by `browser/signin.rs` tests.
