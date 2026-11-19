# Release 0.62.0

Memory release: semantic recall and a contradiction check when facts are
saved, the two gaps found by comparing SeraBot with the multi-agent memory
write-up the owner shared. Tests use synthetic fixtures only. Nothing here
ran against the real E5 model cache or a live model.

## Semantic memory recall

- `memory_vectors.rs` and migration `0042_memory_vectors.sql`: one local
  embedding per fact, from the `[embeddings]` E5 model already used for
  document retrieval. Rows are user-scoped and deleted with their fact.
- A fact's relevance is now the better of its keyword match (BM25) and its
  semantic match (cosine above `semantic_min_similarity`, default 0.82,
  scaled 0..1). A paraphrase with no shared word is now found.
- This covers turn recall (`MemoryHub`), the bots' `memory_search` and
  `memory_list` tools, and the console memory search.
- Vectors are written after saves and edits, and missing or stale ones are
  rebuilt 32 at a time at recall. A missing model or an embedding error falls
  back to keyword recall and never fails a turn.
- `[memory] semantic_recall = false` turns it off.

## Contradiction check

- `runtime/conflicts.rs`: after each owner turn, the facts it saved (by
  extraction or `memory_save`) are compared with the closest older facts.
  Those are semantic neighbours at cosine ≥ 0.86, or keyword matches when no
  vectors exist. One bounded model call says which older facts the new ones
  make untrue.
- The older fact is retired (`superseded`, still restorable). An extracted
  fact that disagrees with something the owner saved is held for review
  instead, so the owner's own words always win.
- The model's answer is untrusted: only ids it was shown are acted on. A
  `memory.conflict` audit row records counts only.
- `[memory] conflict_check = false` turns it off.

## Chat

- `turn.memory` `saved` now lists everything the turn saved, including
  `memory_save` calls, and each item can carry `replaces`. The reply's trace
  shows "Replaced N" and the replaced text, struck through.
- The step row no longer shows the word "Running" next to its spinner.
- The transcript's bottom padding now clears the fade above the composer, so
  the last streamed line is never hidden under it.

## Considered and left out

- Keeping message actions visible at rest (review finding 6): the 0.60
  floating action bar would cover text. It stays hover/focus only. The newest
  reply keeps its actions visible.
- A live elapsed time on running steps needs a per-second repaint, which the
  desktop does not have. The spinner and the turn's "Working…" label cover it.
