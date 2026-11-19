# Mobile (iOS, Android)

Touch first, one-handed reach, compact width, system gestures. Applies to native (SwiftUI, Compose, Slint on Android, freya-android, egui on Android, Dioxus mobile, Tauri 2) and to web on a phone.

## Conventions to check

- **Safe areas**: nothing under the status bar, notch, home indicator, rounded corners, or the keyboard. Inputs scroll into view above the keyboard.
- **Hit targets** 44×44pt iOS, 48×48dp Android, with spacing between adjacent targets.
- **Primary action in thumb reach**: bottom of the screen, full width or right-aligned; never a small text button top-right for the main step.
- **Navigation**: iOS tab bar for 2–5 sections, navigation stack with edge-swipe back kept alive, sheets for self-contained tasks; Android bottom navigation ≤5, predictive back honored, top app bar. Never iOS patterns on Android or the reverse in a single-platform app; for a shared design, decide per component and say which.
- **Text scaling**: Dynamic Type / sp units; layouts survive 200% text. Body 17pt iOS, 16sp Android; floor 11pt / 12sp.
- **System controls** over custom: switches, segmented controls, pickers, action sheets, context menus, swipe actions.
- **Dark appearance** first-class, semantic colors, no raw hex that breaks in dark or high contrast.
- **Permissions and accounts**: ask in context, explain why, allow skipping. Sign-in deferred until it is needed.
- **Offline and poor network** states exist; optimistic UI for frequent writes.
- **Haptics** sparingly, tied to confirmation and boundaries, never decoration.
- **Orientation** handled or locked for a stated reason; foldables and tablets get real layouts, not stretched phone screens.

## Web on a phone

Inputs at 16px to stop zoom, no `maximum-scale`, 100dvh instead of 100vh, no sticky hover states, tap highlight controlled, overscroll contained in sheets, scroll performance with passive listeners. Feel-native tells: a website in a browser still has a 300ms tap delay fixed by `touch-action`, hover-only menus, and desktop-sized targets.

## Motion

Springs and interruptible gestures where the platform does (sheets, swipes); 200 to 350ms for navigation; honor Reduce Motion with crossfades.
