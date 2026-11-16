# Release 0.50.1

Fixes for the v0.49/v0.50 Providers page.

- **Browser sign-in is reachable.** Saving Antigravity, Muse or OpenRouter no longer fails with "secret ... is empty"; members hold their own sealed tokens. Each provider card has a Sign in / Add account button, per-account Sign in again, and a "Save and sign in" path from the Add provider dialog.
- **Antigravity client secret.** Google sign-in needs the OAuth client secret. Paste it once in the write-only field (it goes to the server's encrypted store and is never shown again), or set `AGY_CLIENT_SECRET` on the server. It is not shipped in this public repository.
- **Model lists** follow the owner's HielCowork tables (2026-11-02) and vendor docs; ids that could not be confirmed were left out. Use Fetch models to refresh from the provider.

Not verified: no live sign-in or live model catalog check was run; tests use synthetic fixtures.
