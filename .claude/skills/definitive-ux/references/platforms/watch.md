# Watch (Wear OS; watchOS for reference)

Glanceable, seconds-long sessions, a 1.2 to 1.5 inch display that is often round, driven by touch, a rotary crown or bezel, and one or two hardware buttons. Rust reaches Wear OS through Android (Slint, egui); watchOS is SwiftUI only.

## Conventions to check

- **One thing per screen**: a number, a state, a single action. If the screen needs scrolling to understand, it is two screens.
- **Two taps or fewer** to complete any task; everything else belongs on the phone with a handoff.
- **Round-safe layout**: content inside the inscribed square or a padded circle; nothing in the corners; curved text only for titles. Test on round and square shapes.
- **Text**: body 16 to 18sp minimum, titles 20 to 24sp, high contrast on a black ground (OLED, always-on). No paragraphs.
- **Hit targets** 48dp, ideally full-width rows or large circular buttons; no adjacent small targets.
- **Rotary input** scrolls lists and adjusts values; every scrollable surface responds to it with a visible position indicator.
- **Always-on / ambient mode**: a reduced, mostly black variant of the screen with no animation and no bright fields.
- **Glanceable state** first (tiles and complications on the system surface) rather than opening the app.
- **No text input** beyond voice, presets or a phone handoff; no forms.
- **Haptics** for confirmations and timers; sound rarely.
- **Battery**: no continuous animation, no polling; update on events.

## Motion

Almost none: a crossfade on screen change, a short scale on tap. Nothing over 200ms. Ambient mode has zero motion.
