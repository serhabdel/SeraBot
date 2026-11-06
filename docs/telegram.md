# Telegram

Each user links their own Telegram chat (`/start <code>` with a code from the
console). The linked chat is its own conversation with SeraBot: its id is the
Telegram chat id and its origin channel is `telegram`.

## Conversation origin

Every conversation records where it started (`conversations.origin_channel`,
migration `0018`): `app` (desktop, mobile, API) or `telegram`. Outbound
Telegram delivery is decided by that origin, per user:

| Event | Goes to Telegram? |
| --- | --- |
| Replies, voice replies, progress notes and approval prompts of a `telegram` conversation | Yes, to the owning user's linked chat |
| Handoffs / delegation from a `telegram` conversation (the specialist reports back in the same conversation) | Yes, same chat only |
| Anything produced by an `app` conversation (messages, replies, handoffs, approvals, failure notices) | **No** |
| `SeraBot <version> started` alert (conversation id `system`, no conversation row) | Yes, admin's linked chat |
| Scheduled morning brief and watch runs (conversation id `owner`, no conversation row) | Yes, each owner's own linked chat |
| `/brief` or `/watch` typed in an app conversation | No (it belongs to that app conversation) |
| `telegram_send_document` tool call | Only when the model calls it and the approval is granted; an explicit tool effect, not a mirror |

Telegram conversations still appear in the app's conversation list, so the app
shows what was said in Telegram; the reverse never happens.

Existing installs: migration `0018` marks each user's conversation whose id is
their linked chat id as `telegram`; every other conversation stays `app`.

## Isolation

Routing uses the acting user's own binding and the acting user's own
conversation rows. A user without a linked chat never receives anything, and a
conversation id owned by another user is never treated as a system
notification.
