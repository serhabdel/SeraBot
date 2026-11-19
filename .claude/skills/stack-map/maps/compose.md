<!-- unverified: from Material 3 and public Jetpack Compose API knowledge; not checked against an SDK. -->
# Jetpack Compose map (Android, Wear OS, Android TV)

| Concept | Compose |
| --- | --- |
| Component | `@Composable fun Card(title: String) { .. }`; state with `remember { mutableStateOf(..) }`, `ViewModel` + `StateFlow` |
| Conditional, lists | `if`, `LazyColumn { items(list, key = { it.id }) { .. } }` |
| Spacing | `Column(verticalArrangement = Arrangement.spacedBy(8.dp))`, `Modifier.padding(16.dp)`, `Spacer` |
| Shape | `Modifier.clip(RoundedCornerShape(12.dp))`, `Surface(tonalElevation = 2.dp, shadowElevation = ..)` |
| Typography | `MaterialTheme.typography.bodyLarge`, `sp` units, `maxLines = 1, overflow = TextOverflow.Ellipsis`, `fontFeatureSettings = "tnum"` |
| States | `interactionSource` with `collectIsPressedAsState()`, `enabled = false`, ripple |
| Keyboard and focus | `Modifier.focusable()`, `FocusRequester`, `Modifier.onKeyEvent`, `focusProperties`; TV: `Modifier.focusable()` with a visible focused style and `TvLazyRow` |
| Accessibility | `Modifier.semantics { contentDescription = "Close"; role = Role.Button }`, `Modifier.clearAndSetSemantics`, `mergeDescendants`; TalkBack |
| Motion | `animateFloatAsState`, `AnimatedVisibility`, `animateContentSize`, `updateTransition`, `spring(dampingRatio, stiffness)`, `tween(200, easing = FastOutSlowInEasing)`; honor the animator duration scale |
| Navigation | `NavHost`, `NavController`, bottom `NavigationBar` ≤5, predictive back via `BackHandler` |
| Theme | `MaterialTheme(colorScheme = dynamicDarkColorScheme(..) or darkColorScheme(..))`, semantic color roles, `isSystemInDarkTheme()` |
| Wear OS | `androidx.wear.compose` `ScalingLazyColumn`, `Chip`, rotary via `onRotaryScrollEvent`, round-aware `TimeText` |
| TV | `androidx.tv.material3`, focus-based `Card`s, `TvLazyRow`, overscan padding |
| Verification | Compose previews, `ComposeTestRule`, Accessibility Scanner |

Conventions: `platforms/mobile.md`, `platforms/tv.md`, `platforms/watch.md`.
