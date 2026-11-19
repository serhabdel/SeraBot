---
name: stack-map
description: 'Stack and framework map for UI work in any technology, loaded by every definitive-ux specialist before it names an API. Detects the stack from the project (Cargo.toml for Rust: Freya, egui, gpui, Slint, Iced, Dioxus, Leptos, Yew, ratatui, Tauri; package.json and markup for web; Swift, Kotlin and Flutter for mobile), tells which form factor conventions apply, and loads one verified map that says where spacing, typography, states, focus, accessibility, motion, navigation and theme tokens live in that framework. Rust-native maps are verified against source; others are marked unverified. Use whenever a review or build must write a fix in the project''s own idiom.'
---

# Stack map

Resolve the stack once, say it in the report, load exactly one map (two for hybrids), and write every fix from it. A fix in another stack's idiom is a defect of the review itself.

## 1. Detect

| Evidence | Stack | Map | Form factors |
| --- | --- | --- | --- |
| `freya`, `freya-components` | Freya 0.5 (0.4 uses `rsx!`) | `maps/rust-desktop-core.md` | desktop; mobile via `freya-android`; web via `freya-web` (canvas) |
| `egui`, `eframe` | egui 0.32 | `maps/rust-desktop-core.md` | desktop; android; web (canvas); embedded Linux boards |
| `gpui` | gpui 0.2 (Zed) | `maps/rust-desktop-core.md` | desktop |
| `slint` | Slint 1.x | `maps/rust-slint.md` | desktop, android, embedded MCU and Linux framebuffer, web (wasm) |
| `iced` | Iced 0.13+ | `maps/rust-iced.md` | desktop, web (wasm) |
| `dioxus` | Dioxus 0.6+ | `maps/rust-dioxus.md` + `maps/web.md` | web; desktop and mobile as hybrids (webview) |
| `leptos`, `yew`, `sycamore` | Rust web frameworks | `maps/rust-web.md` + `maps/web.md` | web |
| `ratatui`, `crossterm` | ratatui | `maps/rust-ratatui.md` | tui |
| `tauri` | Tauri 2 | `maps/web.md` for content, `platforms/desktop.md` or `mobile.md` for the shell | hybrid desktop, hybrid mobile |
| `package.json`, `.html`, `.tsx`, `.vue`, `.svelte`, Tailwind | Web | `maps/web.md` | web; hybrid desktop with Electron |
| `*.swift`, `Package.swift`, `.xcodeproj` | SwiftUI / UIKit | `maps/swiftui.md` (unverified) | mobile, desktop (macOS) |
| `build.gradle.kts`, Compose deps | Jetpack Compose | `maps/compose.md` (unverified) | mobile, tv, watch |
| `pubspec.yaml` | Flutter | `maps/flutter.md` (unverified) | mobile, desktop, web |
| `react-native` in package.json, `app.json` | React Native / Expo | `maps/react-native.md` (unverified) | mobile |
| Only a screenshot, image or description | none | no map; fixes in prose, marked as such | as stated by the user |

Hybrids (Tauri, Dioxus desktop, Electron, Capacitor): content is web, the shell is the host platform. Review both and say which finding belongs to which.

Several stacks in one repo (a Rust backend with a Leptos front, a workspace with a Slint HMI and an egui tool): resolve per crate under review.

## 2. Form factor

After the stack, pick the platform conventions from `definitive-ux/references/platforms/`: `desktop`, `mobile`, `tv`, `watch`, `embedded`, `web`, `tui`. A framework can run on several (Slint on an MCU and on a desktop); the form factor comes from the build target, the window config, or the user. Ask only when the answer changes the verdict.

## 3. Verification status

Every map starts with a `verified:` or `unverified:` comment naming the version and the source it was checked against. When a map is unverified, say so in the report and keep fixes to API names you are confident in, or write them in prose. Never invent a method to make a fix look concrete.

Keep maps current by re-checking against the dependency version the project pins; update the header when you do.

## 4. What a map holds

Each map answers, for its framework: component definition, conditional render and list keys, spacing and shape, typography, interaction states, keyboard and focus, accessibility names and roles, motion primitives, navigation, theme tokens, verification commands, and a web-to-framework cheatsheet. The specialists quote those names; nothing else in this plugin hardcodes an API.

## 5. Index

- `maps/rust-desktop-core.md`: Freya, egui, gpui (verified)
- `maps/rust-slint.md`: Slint (verified against builtins.slint and the fluent widget set)
- `maps/rust-iced.md`: Iced (unverified)
- `maps/rust-dioxus.md`: Dioxus (unverified)
- `maps/rust-web.md`: Leptos, Yew (verified for Leptos 0.8 by the user's project)
- `maps/rust-ratatui.md`: ratatui (unverified)
- `maps/web.md`: HTML, CSS, React, Vue, Svelte, Tailwind (verified)
- `maps/swiftui.md`, `maps/compose.md`, `maps/flutter.md`, `maps/react-native.md`: unverified, from platform guidelines
