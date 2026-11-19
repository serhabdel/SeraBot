# Embedded, kiosk and HMI

Fixed display (often 320×240 to 1280×800), touch with gloves or physical buttons and encoders, a microcontroller or a small SoC, no OS font stack, tight RAM and flash, long sessions in bad light. Rust reaches it with Slint (MCU and Linux framebuffer), embedded-graphics, and egui on Linux boards. Also covers kiosks, vending, appliances, instruments and automotive-style HMIs.

## Conventions to check

- **No resize, so design for the exact pixels**: one layout per display, measured, no responsive guessing. Overflow is a defect because there is no scroll fallback the user expects.
- **Touch targets** ≥ 9mm (about 48px at 140 dpi, larger for gloves), with gaps; physical buttons mapped to on-screen labels along the matching edge.
- **Focus for button-driven devices**: a visible cursor, deterministic order, a single Select and a single Back.
- **Latency**: visible feedback within 50ms of any input; screens that take longer show a determinate progress.
- **Readability**: large type (body ≥ 18px on a 7-inch), contrast ≥ 7:1 for daylight, no thin weights, no gradients that band on cheap panels; night mode where the device is used in the dark.
- **States are explicit**: offline, degraded sensor, error with recovery, maintenance, locked. A blank screen or a frozen value is a safety issue.
- **Bundled fonts only**, subsetted, with every glyph the UI needs; no system font assumptions.
- **Memory and frame budget**: no per-frame allocation in the UI loop, textures sized to the panel, animations limited to what the GPU-less renderer can do at the target FPS. Measure on the device, never on the dev machine.
- **Burn-in and wear**: no static bright fields on OLED kiosks; screen dimming after idle.
- **Localization** via bundled catalogs; strings grow 30% in German and shrink in CJK; layouts hold both.
- **Safety-critical actions** (start, stop, dispense, unlock) need a distinct control, a confirmation or a hold-to-confirm, and are never adjacent to routine actions.

## Motion

Minimal and cheap: opacity and position only, 100 to 150ms, disabled entirely below the frame budget. Motion never hides a state change; the value is readable before the animation ends.
