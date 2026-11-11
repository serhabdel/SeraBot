# SeraBot 0.36.0: image and audio generation fixes, composer layout, mobile build

This release closes the v0.36.0 batch: the desktop message box layout (PR #67),
generated media downloads (PR #68), and the Android build check (PR #69).
Checklist: `tasks/big-wave.md`.

## Desktop message box (PR #67)

- **Layout.** The text area spans the full width at the top of the message box.
  The buttons sit in one row below it, and the hint is shown underneath.
- **Focus.** Only the outer box lights up while typing. The text area no longer
  draws its own outline.
- **Limits kept.** The box starts at three lines, grows to 40% of the window,
  and can expand to 70%.

## Generated media (PR #68)

- **Downloads.** A generated image or audio file is checked by its contents, not
  only by its declared type. Results that point to the file, through a redirect
  or a short document, are followed. Redirects are followed only to hosts on the
  approved list.
- **No repeated charges.** A result that cannot be saved is refused once and the
  task is not run again, so a finished generation is not paid for twice.
- **Credentials.** Generation identifiers are shown as references only. The
  saved sign-in keeps its own token.

## Android build (PR #69)

- The Android app uses the current Flutter widget package, which replaces the
  deprecated Material import. The Android build tooling is updated to match.
  The Android continuous-integration check passes again.

## Versions

- Server and desktop workspace: 0.36.0 (`Cargo.toml`, `Cargo.lock`). CI creates
  the `v0.36.0` tag from the workspace version.
- Mobile app: `pubspec.yaml` is unchanged.

## Not yet verified

- Desktop: the message box on Windows, in light and dark themes.
- Generated media: one real generation on a connected account, and the provider
  credit count after it.
- Android: a run on a device.
