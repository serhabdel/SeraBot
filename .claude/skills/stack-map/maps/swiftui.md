<!-- unverified: from Apple HIG and public SwiftUI API knowledge; not checked against an SDK. Quote only names you are certain of; otherwise prose. -->
# SwiftUI map (iOS, iPadOS, macOS)

| Concept | SwiftUI |
| --- | --- |
| Component | `struct Card: View { var body: some View { .. } }`; `@State`, `@Binding`, `@Observable` |
| Conditional, lists | `if`, `ForEach(items, id: \.id)`, `List` |
| Spacing | `VStack(spacing: 8)`, `.padding(16)`, `.padding(.horizontal, 24)`, `Spacer()` |
| Shape | `.clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))`, `.shadow(radius:y:)`, `.background(.regularMaterial)` |
| Typography | `.font(.body)` text styles (Dynamic Type), `.fontWeight`, `.lineLimit(1)`, `.truncationMode(.tail)`, `.monospacedDigit()` |
| States | `Button` styles with `configuration.isPressed`, `.disabled(..)`, `.hoverEffect()` on iPad and Mac |
| Keyboard and focus | `@FocusState`, `.focused(..)`, `.keyboardShortcut("s", modifiers: .command)`, `.onKeyPress` |
| Accessibility | `.accessibilityLabel("Close")`, `.accessibilityHint`, `.accessibilityAddTraits(.isButton)`, `.accessibilityHidden(true)`, `.accessibilityElement(children: .combine)` |
| Motion | `withAnimation(.spring(duration: 0.3, bounce: 0))`, `.animation(.easeOut(duration: 0.2), value:)`, `.transition(.opacity)`, `matchedGeometryEffect`; honor `accessibilityReduceMotion` |
| Navigation | `NavigationStack`, `TabView`, `.sheet`, `.fullScreenCover`, `.popover`, `.confirmationDialog` |
| Theme | asset catalog colors with light/dark variants, semantic `Color.primary` etc., `.tint(..)`, `@Environment(\.colorScheme)` |
| Platform | safe areas automatic; `.toolbar`, menu bar via `.commands` on macOS; Settings scene |
| Verification | Xcode previews, `XCTest` UI tests, Accessibility Inspector |

Conventions: `platforms/mobile.md`, `platforms/desktop.md`.
