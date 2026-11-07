import 'package:flutter/material.dart';

import '../session.dart';
import '../update_flow.dart';
import '../validation.dart';
import 'approvals_tab.dart';
import 'chat_tab.dart';
import 'fleet_tab.dart';
import 'voice_tab.dart';

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.session});

  final OwnerSession session;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  int _index = 0;

  Future<void> _signOut() => widget.session.signOut();

  @override
  Widget build(BuildContext context) {
    final version = widget.session.serverVersion;
    final user = widget.session.user;
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('SeraBot'),
            if (user != null)
              Text(
                user.label,
                style: Theme.of(context).textTheme.bodySmall,
                overflow: TextOverflow.ellipsis,
              ),
          ],
        ),
        actions: [
          if (version != null && version.isNotEmpty)
            Center(
              child: Padding(
                padding: const EdgeInsets.only(right: 8),
                child: Text('v$version', style: Theme.of(context).textTheme.bodySmall),
              ),
            ),
          IconButton(
            tooltip: 'Check for updates',
            icon: const Icon(Icons.system_update),
            onPressed: () => checkForUpdates(context),
          ),
          TextButton(onPressed: _signOut, child: const Text('Sign out')),
        ],
      ),
      body: IndexedStack(
        index: _index,
        children: [
          ChatTab(session: widget.session),
          ApprovalsTab(session: widget.session),
          FleetTab(
            session: widget.session,
            onUseInChat: (id) {
              if (!validId(id)) {
                return;
              }
              widget.session.setConversationId(id);
              setState(() => _index = 0);
            },
          ),
          VoiceTab(session: widget.session),
        ],
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (i) => setState(() => _index = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.chat), label: 'Chat'),
          NavigationDestination(icon: Icon(Icons.gavel), label: 'Approvals'),
          NavigationDestination(icon: Icon(Icons.account_tree), label: 'Fleet'),
          NavigationDestination(icon: Icon(Icons.graphic_eq), label: 'Voice'),
        ],
      ),
    );
  }
}
