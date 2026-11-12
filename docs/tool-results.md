# Structured tool results (0.37)

A tool still returns text in `content` for the model. Beside that text it may
return `cards`: a list of typed objects clients render without parsing the
prose. The same list is copied onto the `turn.step` event (see
[Events](events.md)) when the content policy accepts it.

ElevenLabs sets cards itself. Other tools get cards derived from the artifact
hash and filename, from `data.artifacts`, from `url` / `hits` / `results` /
`links`, and from a `columns` + `rows` table (or `data.table`). At most eight
cards. HTTPS links only: no userinfo, no IP hosts. Artifact downloads use the
owner API path `/v1/artifacts/{sha256}`, which still requires the owner's
token. The path is not a public URL.

## Card types

`type` is the discriminator.

### `audio`

```json
{
  "type": "audio",
  "url": "/v1/artifacts/…",
  "voice": "rachel",
  "model": "eleven_flash",
  "duration_ms": 2500,
  "artifact_hash": "…",
  "filename": "line.mp3",
  "mime": "audio/mpeg",
  "title": "Line"
}
```

`voice`, `model`, and `duration_ms` come from the call arguments (voice name
or id, model id, `duration_ms` or `duration_seconds`). The prompt is not
copied. A generation that is still rendering has no hash; `title` is
`Generating audio` and the step phase is `progress`.

### `file`

```json
{
  "type": "file",
  "url": "/v1/artifacts/…",
  "artifact_hash": "…",
  "filename": "notes.docx",
  "mime": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "bytes": 12000,
  "title": "Notes"
}
```

### `image`

```json
{
  "type": "image",
  "url": "/v1/artifacts/…",
  "artifact_hash": "…",
  "filename": "chart.png",
  "mime": "image/png",
  "alt": "Chart"
}
```

### `link`

```json
{
  "type": "link",
  "url": "https://example.com/a",
  "title": "A",
  "description": "Short snippet"
}
```

### `table`

```json
{
  "type": "table",
  "title": "Parts",
  "columns": ["Name", "Qty"],
  "rows": [["Bolt", "2"]]
}
```

At most 8 columns and 12 rows. Cells are clipped to 80 characters.

A tool that wants a specific card sets `ToolOutput.cards` in
`crates/serabot/src/result_cards.rs` (`ResultCard`). Derived cards are added
only for hashes and links that are not already present.
