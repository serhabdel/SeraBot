import 'package:flutter/material.dart';

import 'screens/connect_screen.dart';
import 'screens/home_screen.dart';
import 'session.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(SeraBotMobileApp(session: OwnerSession()));
}

class SeraBotMobileApp extends StatefulWidget {
  const SeraBotMobileApp({super.key, required this.session});

  final OwnerSession session;

  @override
  State<SeraBotMobileApp> createState() => _SeraBotMobileAppState();
}

class _SeraBotMobileAppState extends State<SeraBotMobileApp> {
  bool _restoring = true;

  OwnerSession get _session => widget.session;

  @override
  void initState() {
    super.initState();
    _session.addListener(_onChanged);
    _session.restore().whenComplete(() {
      if (mounted) {
        setState(() => _restoring = false);
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
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF1B4332)),
        useMaterial3: true,
        materialTapTargetSize: MaterialTapTargetSize.padded,
      ),
      home: home,
    );
  }
}
