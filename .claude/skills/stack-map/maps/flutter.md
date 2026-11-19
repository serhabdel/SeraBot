<!-- unverified: from public Flutter API knowledge; not checked against an SDK. -->
# Flutter map (mobile, desktop, web)

| Concept | Flutter |
| --- | --- |
| Component | `class Card extends StatelessWidget` / `StatefulWidget`; `build(BuildContext)`; state via `setState`, Riverpod, Bloc |
| Conditional, lists | `if (cond) Widget`, `ListView.builder(itemCount:, itemBuilder:)`, keys `ValueKey(id)` |
| Spacing | `Padding(padding: EdgeInsets.all(16))`, `SizedBox(height: 8)`, `Column(spacing: 8)` (3.27+), `Gap` |
| Shape | `Container(decoration: BoxDecoration(borderRadius: BorderRadius.circular(12), boxShadow: [..]))`, `Card`, `Material(elevation:)` |
| Typography | `Theme.of(context).textTheme.bodyLarge`, `Text(maxLines: 1, overflow: TextOverflow.ellipsis)`, `textScaler` for scaling, `FontFeature.tabularFigures()` |
| States | `MaterialStateProperty` / `WidgetStateProperty` for hovered, pressed, focused, disabled; `InkWell` |
| Keyboard and focus | `Focus`, `FocusNode`, `Shortcuts` + `Actions`, `FocusTraversalGroup`; TV: `Focus` with a visible focused decoration |
| Accessibility | `Semantics(label:, button: true, excludeSemantics:)`, `MergeSemantics`, `ExcludeSemantics`; `Tooltip` doubles as a label |
| Motion | `AnimatedContainer`, `AnimatedOpacity`, `AnimationController` + `CurvedAnimation(curve: Curves.easeOutCubic)`, `Hero`, implicit animations; `MediaQuery.disableAnimations` |
| Navigation | `Navigator 2.0` / `go_router`, `BottomNavigationBar` ≤5, `showModalBottomSheet`, `showDialog` |
| Theme | `ThemeData(colorScheme: ColorScheme.fromSeed(..))`, `ThemeMode.system`, extensions for custom tokens |
| Platform | `SafeArea`, `Platform.isIOS` for adaptive widgets (`Cupertino*`), `MediaQuery.sizeOf` for breakpoints; desktop: `MenuBar`, `PlatformMenuBar` on macOS |
| Verification | `flutter test` widget tests, golden tests, `flutter run -d` on device |

Conventions: `platforms/mobile.md`, `platforms/desktop.md`, `platforms/web.md`.
