<!-- unverified: from public React Native and Expo API knowledge; not checked against an SDK. -->
# React Native / Expo map (mobile)

| Concept | React Native |
| --- | --- |
| Component | function components, hooks; `View`, `Text`, `Pressable`, `TextInput`, `FlatList` |
| Conditional, lists | JSX conditionals, `FlatList` with `keyExtractor`, `getItemLayout` for performance |
| Spacing | `StyleSheet` with `padding: 16`, `gap: 8`, flexbox everywhere |
| Shape | `borderRadius`, `shadowColor/Offset/Opacity/Radius` (iOS), `elevation` (Android) |
| Typography | `fontSize`, `lineHeight`, `allowFontScaling` (keep true), `numberOfLines={1}` with `ellipsizeMode`, `fontVariant: ['tabular-nums']` |
| States | `Pressable` style callback `({ pressed }) => ..`, `disabled` |
| Keyboard | `KeyboardAvoidingView`, `returnKeyType`, `onSubmitEditing`; TV: `hasTVPreferredFocus`, `TVEventHandler` |
| Accessibility | `accessible`, `accessibilityLabel`, `accessibilityRole="button"`, `accessibilityState`, `accessibilityHint`; `importantForAccessibility="no"` for decoration |
| Motion | `react-native-reanimated` (`withSpring`, `withTiming`, `useSharedValue` on the UI thread), `Animated` legacy; `react-native-gesture-handler` for gestures; `AccessibilityInfo.isReduceMotionEnabled` |
| Navigation | `@react-navigation/native` stack and bottom tabs (≤5), `expo-router` file routes; native back gesture kept |
| Theme | a theme object in context, `useColorScheme()` for dark, semantic names |
| Platform | `SafeAreaView` / `react-native-safe-area-context`, `Platform.select`, native wallet buttons from the payment SDK |
| Verification | Expo Go or dev client on device, Jest with `@testing-library/react-native`, Detox |

Conventions: `platforms/mobile.md`.
