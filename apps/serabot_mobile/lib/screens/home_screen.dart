import 'package:flutter/material.dart';

import '../session.dart';
import 'approvals_tab.dart';
import 'chat_tab.dart';

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.session, required this.onDisconnect});

  final OwnerSession session;
  final VoidCallback onDisconnect;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  int _index = 0;

  void _disconnect() {
    widget.session.disconnect();
    widget.onDisconnect();
  }

  @override
  Widget build(BuildContext context) {
    final version = widget.session.serverVersion;
    return Scaffold(
      appBar: AppBar(
        title: const Text('SeraBot'),
        actions: [
          if (version != null && version.isNotEmpty)
            Center(
              child: Padding(
                padding: const EdgeInsets.only(right: 8),
                child: Text('v$version', style: Theme.of(context).textTheme.bodySmall),
              ),
            ),
          TextButton(onPressed: _disconnect, child: const Text('Disconnect')),
        ],
      ),
      body: IndexedStack(
        index: _index,
        children: [
          ChatTab(session: widget.session),
          ApprovalsTab(session: widget.session),
        ],
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (i) => setState(() => _index = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.chat), label: 'Chat'),
          NavigationDestination(icon: Icon(Icons.gavel), label: 'Approvals'),
        ],
      ),
    );
  }
}
