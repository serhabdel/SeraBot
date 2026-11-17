# Release 0.53.0

One look across the console, calmer motion, and provider changes that need no
restart.

## Providers apply without a restart

The model router is now shared through one swappable handle (`RouterHandle`):
the runtime, delegation, team and bot-management tools all read the live
router. Saving or removing a provider in the console rebuilds the router from
the live provider list (`ModelRouter::rebuilt` keeps per-user credentials, the
in-flight gates, and the cached clients and model catalogs of providers that
did not change) and swaps it in. `POST /v1/admin/providers` and `DELETE
/v1/admin/providers/{id}` answer `restart_required: false`, and the console says
"Saved. Your bots can use it now." A turn already running keeps the router it
started with. If the new list fails the startup checks, the running router
stays, `pending_restart` stays true and the card offers **Restart server**, so a
bad save never takes working providers down.

## Shared surface system

`assets/system.css`, loaded last, settles what every page shares:

- one card radius, shadow and hover lift for pressable cards;
- stat tiles (value first, label under it) for Tasks, Workflows, Vault and Usage;
- segmented controls that never wrap a label;
- one softer skeleton shimmer;
- list and grid entry motion (a short rise, 28 ms apart for the first eight items);
- one focus ring on pressable cards;
- everything stops under `prefers-reduced-motion` and the console's own
  reduce-motion switch.

## Page polish

- **Tasks:** a real view switcher, stat tiles, card-shaped skeletons, proper
  empty states, a collapsible Completed list and keyboard-reachable calendar days.
- **Workflows:** stat tiles in the body, section counts, one empty-state
  component, a Retry on errors.
- **Logs:** a compact level filter, marked warn and error lines, skeleton rows.
- **Memory:** search and filters first, memory cards with an importance meter,
  and **Add memory** as a header button that opens a compact composer
  (Ctrl+Enter saves, Esc closes).
- **Vault:** stat tiles only once loaded (no false zeros), a proper error state
  with Try again, section heads and loading cards. Values stay write-only.
- **Usage:** clear sections (Quota, Reliability, Activity, Breakdown, This
  computer), a daily chart with gridlines, side-by-side breakdowns.
- **Settings:** a card-style navigation with an accent bar; a non-test
  `expect` replaced by a safe return.
- **Integrations and Marketplace:** aligned toolbar, equal-height cards with
  the same anatomy, skeleton cards that show that anatomy while loading.
- **Browser:** a "Waiting for a picture" placeholder, loading cards, and no
  endless skeleton when the list failed to load.
- **Sheets and sign-in:** one head, body and foot treatment, a slide-in for the
  side sheet, a centred sign-in card.

## Not verified

- Screens were checked in a local render of the real components with synthetic
  data (dark and light), not on a Windows device; motion was reviewed in CSS.
- No provider was used live; the router swap is covered by synthetic tests.
