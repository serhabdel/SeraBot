# Biases, defaults & dark patterns

These laws work below conscious deliberation. Recommend the user-serving version; flag manipulative ones as Defects (they erode trust and can be illegal, e.g. pre-ticked consent under GDPR).

| Legitimate | Dark pattern to flag |
|---|---|
| Recommended plan that fits most users | Pre-selected paid add-ons |
| Privacy-protective defaults | Pre-checked marketing/tracking opt-in |
| Real scarcity or deadlines | Fake timers, fake "only 2 left" |
| Neutral, equal-weight decline option | Confirmshaming ("No, I don't like saving money") |
| Cancel as easy as signup | Roach motel (hard to cancel/delete) |
| Progress reflecting real steps | Fake progress, endless funnels |
| Clear price up front | Hidden fees revealed at the last step |

Stack-agnostic. In code, look for consent state initialized to `true`, decline options styled weaker than accept (smaller font, muted color, text link vs filled button), cancel flows that need more steps than signup, and prices computed in a later step than the one that shows "total".
