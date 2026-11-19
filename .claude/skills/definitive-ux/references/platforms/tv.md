# TV (Android TV, Fire TV, tvOS-class 10-foot UI)

Viewed from 3 meters, driven by a D-pad or remote with five keys and Back. No pointer, no touch, no hover. Rust reaches it through Android (Slint, egui, Dioxus, freya-android); tvOS is SwiftUI only.

## Conventions to check

- **Focus is the cursor.** Exactly one focused element at all times, always visible: scale 1.05 to 1.1 plus a bright border or a lifted surface. Losing focus (an element disappears, a screen changes) is Blocking: focus must land somewhere predictable.
- **D-pad grid**: up/down/left/right move between visible neighbors in the direction pressed; no traps, no dead ends, no diagonal jumps. Rows of cards scroll horizontally, keeping the focused item in a stable column.
- **Back** goes one level up or to the previous screen; from the home screen it exits. Never repurpose Back.
- **Overscan**: 5% safe margin on every edge; nothing interactive or informative in the outer band.
- **Scale**: text ≥ 24px at 1080p (body 28 to 32, titles 48 to 64), hit items ≥ 100px tall; thin rules and 1px borders vanish.
- **Content leads**: posters and stills carry the screen; chrome minimal; no dense tables; no forms longer than three fields (voice or phone handoff for text entry).
- **Rows, not grids of tiny tiles**: horizontal rails with a title, 5 to 7 items visible, predictable ordering.
- **Playback controls** appear on any key press and auto-hide; scrubbing with left/right and long-press; a large time display.
- **Dark surfaces** by default (living rooms, OLED); contrast ≥ 4.5:1 and no pure white fields.
- **No hover, no tooltips, no right-click**: every affordance is visible or revealed on focus.
- **Latency**: focus moves must respond under 100ms; a slow focus move feels broken on a remote.

## Motion

Focus transitions 150 to 250ms, ease-out, with the focused item scaling up; rail scrolling animated so the eye follows; no entrance animations per screen beyond a crossfade. Motion carries spatial continuity here more than anywhere else, and it must never delay the next key press.
