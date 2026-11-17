# Release 0.52.0

Sign-in that works, an AI providers page, a calmer chat, and bots anyone can
add from the marketplace.

## Sign-in fixes

- **Sign in with Claude** works in one press. Saving an Anthropic provider from
  the console no longer asks for an API key while its plan sign-in is on (the
  save used to fail with "secret anthropic_api_key is empty", so the sign-in
  never opened). A console provider saved before 0.50.1 with plan sign-in off
  now says so, and the admin's **Turn on Claude sign-in** saves it again.
  Members see that Claude sign-in is for the admin, with the API-key path,
  instead of a missing button.
- **Google Antigravity**: a server built without the Google client secret used
  to send members through Google's consent and then fail at the code exchange.
  Sign-in now refuses up front (`422 signin_not_set_up`), the card asks the
  admin for the secret (saved encrypted, no restart), and `deploy-ovh.yml`
  passes the repository secret `SERABOT_AGY_CLIENT_SECRET` to the server build.
  **Owner action:** add that repository secret, or paste the secret once on the
  AI providers page.
- **Restart prompt**: the model router reads providers at start. A provider
  first saved from the console could be signed into but not used by bots until
  a restart nobody was asked for. Cards and the "Account added" notice now say
  so and offer **Restart server**.
- **Codex (ChatGPT)** was checked end to end (preset, server models, browser
  and device sign-in, Responses transport, refresh) and needed no change beyond
  the restart prompt.
- Provider listings carry `plan_signin`, `signin_setup` and `pending_restart`
  ([API](api.md)).

## AI providers page

"AI keys" is renamed **AI providers** and rebuilt: provider cards with their
accounts and inline fixes, a default-model strip, then (admin) **Sign in with an
account** tiles for Claude, ChatGPT, Antigravity, Copilot, Muse and OpenRouter
and an **Use an API key** grid that offers only providers not added yet.

## Chat, sidebar and All chats

Soft tinted avatar tiles, the latest message under each bot, quieter group
labels, row menus on hover, the full bot name in the topbar with search moved
into it, tighter messages, a one-line composer at rest, and a searchable All
chats list.

## Marketplace bots

Seven catalog-only bots that every member can add from the **Bots** tab:
Researcher, Writer, Translator, Planner, Support Writer, Product Writer and
Tutor ([details](skills.md)). They are not added to anyone's team on their
own. The Bots tab is now a card grid with a **New** badge.

## Not verified

- No sign-in was run against a live Google, Anthropic, OpenAI or GitHub account;
  everything is checked against synthetic fixtures.
- The desktop screens were checked in a local render of the real components
  (dark and light), not on a Windows device.
