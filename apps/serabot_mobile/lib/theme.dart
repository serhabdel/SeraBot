import 'package:material_ui/material_ui.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// Brand violet used by the desktop console's default light theme.
const seraViolet = Color(0xFF6D4AFF);

ThemeData buildTheme(Brightness brightness) {
  final scheme = ColorScheme.fromSeed(
    seedColor: seraViolet,
    brightness: brightness,
  );
  return ThemeData(
    colorScheme: scheme,
    useMaterial3: true,
    materialTapTargetSize: MaterialTapTargetSize.padded,
    scaffoldBackgroundColor: brightness == Brightness.light
        ? const Color(0xFFFAF8FF)
        : scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      surfaceTintColor: scheme.surfaceTint,
      centerTitle: false,
    ),
    inputDecorationTheme: InputDecorationTheme(
      border: OutlineInputBorder(borderRadius: BorderRadius.circular(24)),
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      color: scheme.surfaceContainerLow,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16)),
    ),
  );
}

/// Page route builder with no animation, used when the system asks for
/// reduced motion (Android "Remove animations").
class NoTransitionsBuilder extends PageTransitionsBuilder {
  const NoTransitionsBuilder();

  @override
  Widget buildTransitions<T>(
    PageRoute<T> route,
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
    Widget child,
  ) =>
      child;
}

/// [data] with page transitions switched off.
ThemeData withoutPageTransitions(ThemeData data) => data.copyWith(
      pageTransitionsTheme: const PageTransitionsTheme(
        builders: {
          TargetPlatform.android: NoTransitionsBuilder(),
          TargetPlatform.iOS: NoTransitionsBuilder(),
          TargetPlatform.linux: NoTransitionsBuilder(),
          TargetPlatform.macOS: NoTransitionsBuilder(),
          TargetPlatform.windows: NoTransitionsBuilder(),
          TargetPlatform.fuchsia: NoTransitionsBuilder(),
        },
      ),
    );

/// Where the theme choice is kept (not a secret; any small key-value store).
abstract class ThemePrefs {
  Future<String?> read();
  Future<void> write(String value);
}

class SecureThemePrefs implements ThemePrefs {
  static const _key = 'serabot.theme';
  final _s = const FlutterSecureStorage();
  @override
  Future<String?> read() => _s.read(key: _key);
  @override
  Future<void> write(String value) => _s.write(key: _key, value: value);
}

class MemoryThemePrefs implements ThemePrefs {
  String? value;
  @override
  Future<String?> read() async => value;
  @override
  Future<void> write(String v) async => value = v;
}

/// Light (default), dark, or follow the system.
class ThemeController extends ChangeNotifier {
  ThemeController({ThemePrefs? prefs}) : _prefs = prefs ?? SecureThemePrefs();

  final ThemePrefs _prefs;
  ThemeMode _mode = ThemeMode.light;
  ThemeMode get mode => _mode;

  Future<void> load() async {
    try {
      final v = await _prefs.read();
      _mode = parseThemeMode(v) ?? ThemeMode.light;
      notifyListeners();
    } catch (_) {
      // Keep the default.
    }
  }

  Future<void> set(ThemeMode mode) async {
    _mode = mode;
    notifyListeners();
    try {
      await _prefs.write(mode.name);
    } catch (_) {}
  }
}

ThemeMode? parseThemeMode(String? v) {
  for (final m in ThemeMode.values) {
    if (m.name == v) {
      return m;
    }
  }
  return null;
}
