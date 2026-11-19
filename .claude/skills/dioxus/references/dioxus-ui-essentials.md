# Dioxus UI Essentials (0.7)

Reference snapshot of <https://dioxuslabs.com/learn/0.7/essentials/ui/> (captured for offline agent use).

Welcome to the Dioxus "Building User Interfaces" chapter. It teaches how to build beautiful user interfaces in Rust and Dioxus.

## Key points

- Dioxus lets you write UI in HTML and CSS. On the web the UI is native and accessible; on desktop and mobile widgets are rendered in a webview or natively with hybrid native components.
- **What Dioxus gives you:**
  - Hot-reloading for Rust code, UI, styles, and assets
  - Cross-platform renderers for web, desktop, and mobile
  - Reactivity system for managing and updating state
  - Growing SDK of cross-platform APIs
  - Backend integration for building fullstack web apps
  - Tools for bundling and deploying to production
- **What HTML and CSS give you:**
  - Multiple layout algorithms (flexbox, grid, table, float, block, etc.)
  - Stylesheets for app-level theming, per-element styling
  - Great accessibility and screen-reader support
  - Inline SVG support, CSS animations
  - Multimedia elements like `img`, `video`, and `audio`
- **What Rust gives you:**
  - Standardized tooling (cargo, rustc), crates.io ecosystem
  - Runs everywhere: Mac, Windows, Linux, iOS, Android, Web
  - Strong type system preventing many logic bugs at compile time
  - Performance rivaling C/C++, reliable at scale
- Drawbacks: slower compile times, steep learning curve, ergonomic papercuts; the Dioxus team works on "high-level Rust" (hot-patching, automatic ref-counting).
