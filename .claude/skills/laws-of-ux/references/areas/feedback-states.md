# Feedback, performance & states

- Every action acknowledged within ~100 ms (pressed state, optimistic update). — Doherty
- > ~1 s: loading indicator, disable re-submit (prevents double charges). > ~10 s: determinate progress + cancel/background. — Doherty
- Skeletons / optimistic UI over blank screens and full-page spinners. Prefetch on intent. — Doherty
- Async handlers need loading, error and success branches; a click that awaits with no UI change is a defect. — Doherty, Peak-End
- Empty states explain what goes here and offer the first action (not rows of zeros). — Paradox of the Active User
- Errors: human, specific, next to the cause, with a recovery path. They're often the "peak" users remember. — Peak-End
- Success/end states: confirmation, summary, clear next step. — Peak-End
- Destructive actions: undo preferred, confirm otherwise. — Peak-End
- Background work (sync, upload, saving) visibly reported. — Zeigarnik

**Rust native pointers:** async results are `FutureState::{Pending, Loading, Fulfilled}` or `QueryStateData` in Freya, a manual `enum` or `poll_promise` in egui, a `Task` plus `cx.notify()` in gpui. A `match` with only the `Fulfilled`/`Ok` arm rendered is a defect. Background work surfaces through a `Loader`, `ProgressBar`, `spinner()` or a status strip; a button that disables itself without a visible reason is a Medium.
