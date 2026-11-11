# SeraBot 0.35.0: several chats per bot, steadier chat, Tasks fix

This release closes the v0.35.0 batch: server work for chats per bot, memory
refresh and the Tasks fix (PR #66), and the desktop chat experience (PR #65).
Checklist: `tasks/big-wave.md`.

## Server (PR #66)

- **Several chats per bot.** Start a new chat with the same bot and switch
  between its chats. A new chat keeps the bot's saved memories and starts with
  a short summary of the earlier chat, not its transcript.
- **Memory refresh.** After every N of your messages (default 40, set per bot
  from 5 to 500) the recent turns are summarised and their lasting facts are
  saved for your review, in the background after the reply.
- **Tasks.** Opening Tasks no longer fails when no calendar is chosen. Refused
  requests name the field that was wrong.

## Desktop chat (PR #65)

- **Jump to latest** is steady: the page tracks whether you are following the
  conversation and only stops following when you scroll up.
- **Composer.** Larger, grows with your text, has an expand button and the
  `/new`, `/memory`, `/attach`, `/stop` and `/expand` commands.
- **Sidebar.** Each bot shows its chats with New chat, switching, rename and
  archive with Undo. Row actions are in one menu, a right-click menu and the
  chat header menu instead of hover icons.
- **Bot settings.** A Memory refresh setting per bot.

## Not covered

- Manual Windows checks for the chat, sidebar and composer (see the PR checklists).
