import 'package:flutter/material.dart';

import 'screens/connect_screen.dart';
import 'screens/home_screen.dart';
import 'session.dart';
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
  });

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

  @override
  void initState() {
    super.initState();
    _session.addListener(_onChanged);
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
    _session.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final Widget home;
    if (_restoring) {
      home = const Scaffold(body: Center(child: CircularProgressIndicator()));
    } else if (_session.connected) {
      home = HomeScreen(session: _session);
    } else {
      home = ConnectScreen(session: _session);
    }
    return MaterialApp(
      title: 'SeraBot',
      navigatorKey: _navigatorKey,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF1B4332)),
        useMaterial3: true,
        materialTapTargetSize: MaterialTapTargetSize.padded,
      ),
      home: home,
    );
  }
}
