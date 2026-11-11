import 'package:material_ui/material_ui.dart';

import 'screens/connect_screen.dart';
import 'screens/home_screen.dart';
import 'device.dart';
import 'session.dart';
import 'theme.dart';
import 'update_flow.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(SeraBotMobileApp(session: OwnerSession(), checkUpdatesOnStart: true));
}

class SeraBotMobileApp extends StatefulWidget {
  const SeraBotMobileApp({
    super.key,
    required this.session,
    this.checkUpdatesOnStart = false,
    this.theme,
    this.device,
    this.startEvents = true,
  });

  /// Test hooks: theme store, phone features, and the live event stream.
  final ThemeController? theme;
  final DeviceActions? device;
  final bool startEvents;

  final OwnerSession session;

  /// Silent update check after startup. Off in tests (needs platform plugins).
  final bool checkUpdatesOnStart;

  @override
  State<SeraBotMobileApp> createState() => _SeraBotMobileAppState();
}

class _SeraBotMobileAppState extends State<SeraBotMobileApp> {
  bool _restoring = true;
  final _navigatorKey = GlobalKey<NavigatorState>();

  OwnerSession get _session => widget.session;
  late final ThemeController _theme = widget.theme ?? ThemeController();

  @override
  void initState() {
    super.initState();
    _session.addListener(_onChanged);
    _theme.addListener(_onChanged);
    _theme.load();
    _session.restore().whenComplete(() {
      if (mounted) {
        setState(() => _restoring = false);
        if (widget.checkUpdatesOnStart) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            final ctx = _navigatorKey.currentContext;
            if (ctx != null) {
              checkForUpdates(ctx, silent: true);
            }
          });
        }
      }
    });
  }

  void _onChanged() {
    if (mounted) {
      setState(() {});
    }
  }

  @override
  void dispose() {
    _session.removeListener(_onChanged);
    _theme.removeListener(_onChanged);
    _session.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final Widget home;
    if (_restoring) {
      home = const Scaffold(body: Center(child: CircularProgressIndicator()));
    } else if (_session.connected) {
      home = HomeScreen(
        key: ValueKey(_session.user?.id ?? 'home'),
        session: _session,
        theme: _theme,
        device: widget.device,
        startEvents: widget.startEvents,
      );
    } else {
      home = ConnectScreen(session: _session);
    }
    return MaterialApp(
      title: 'SeraBot',
      navigatorKey: _navigatorKey,
      theme: buildTheme(Brightness.light),
      darkTheme: buildTheme(Brightness.dark),
      themeMode: _theme.mode,
      builder: (context, child) {
        final content = child ?? const SizedBox.shrink();
        if (!MediaQuery.disableAnimationsOf(context)) {
          return content;
        }
        return Theme(
          data: withoutPageTransitions(Theme.of(context)),
          child: content,
        );
      },
      home: home,
    );
  }
}
