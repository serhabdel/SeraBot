import 'package:flutter/material.dart';

import 'screens/connect_screen.dart';
import 'screens/home_screen.dart';
import 'session.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const SeraBotMobileApp());
}

class SeraBotMobileApp extends StatefulWidget {
  const SeraBotMobileApp({super.key});

  @override
  State<SeraBotMobileApp> createState() => _SeraBotMobileAppState();
}

class _SeraBotMobileAppState extends State<SeraBotMobileApp> {
  final _session = OwnerSession();
  bool _connected = false;

  @override
  void dispose() {
    _session.disconnect(notify: false);
    _session.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'SeraBot',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF1B4332)),
        useMaterial3: true,
      ),
      home: _connected
          ? HomeScreen(
              session: _session,
              onDisconnect: () => setState(() => _connected = false),
            )
          : ConnectScreen(
              session: _session,
              onConnected: () => setState(() => _connected = true),
            ),
    );
  }
}
