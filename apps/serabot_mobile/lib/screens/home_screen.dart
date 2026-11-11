import 'package:material_ui/material_ui.dart';

import '../app_state.dart';
import '../device.dart';
import '../session.dart';
import '../theme.dart';
import 'approvals_screen.dart';
import 'bots_screen.dart';
import 'chats_screen.dart';
import 'settings_screen.dart';
import 'workflows_screen.dart';

/// Signed-in shell: Chats, Approvals (badge), Bots, Settings. It owns the
/// [AppState] so the conversation list, unread counts and event stream live
/// as long as the user is signed in.
class HomeScreen extends StatefulWidget {
  const HomeScreen({
    super.key,
    required this.session,
    required this.theme,
    this.device,
    this.startEvents = true,
  });

  final OwnerSession session;
  final ThemeController theme;
  final DeviceActions? device;

  /// Tests turn the live stream off.
  final bool startEvents;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  int _index = 0;
  late final AppState _app;
  late final DeviceActions _device;

  @override
  void initState() {
    super.initState();
    _app = AppState(widget.session.client);
    _device = widget.device ?? PlatformDeviceActions();
    _app.start(events: widget.startEvents);
  }

  @override
  void dispose() {
    _app.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: _app,
      builder: (context, _) {
        final unread = _app.unread;
        final pending = _app.pendingApprovals;
        return Scaffold(
          body: ListenableBuilder(
            listenable: widget.session,
            builder: (context, _) {
              final failed = widget.session.saveFailed;
              return Column(
                children: [
                  if (failed)
                    SafeArea(
                      bottom: false,
                      child: _SaveFailedBanner(session: widget.session),
                    ),
                  Expanded(
                    child: MediaQuery.removePadding(
                      context: context,
                      removeTop: failed,
                      child: IndexedStack(
                        index: _index,
                        children: [
                          ChatsScreen(app: _app, device: _device),
                          ApprovalsScreen(app: _app),
                          WorkflowsScreen(app: _app),
                          BotsScreen(app: _app, device: _device),
                          SettingsScreen(
                            session: widget.session,
                            app: _app,
                            theme: widget.theme,
                          ),
                        ],
                      ),
                    ),
                  ),
                ],
              );
            },
          ),
          bottomNavigationBar: NavigationBar(
            selectedIndex: _index,
            onDestinationSelected: (i) => setState(() => _index = i),
            destinations: [
              NavigationDestination(
                icon: Badge(
                  isLabelVisible: unread > 0,
                  label: Text(unread > 99 ? '99+' : '$unread'),
                  child: const Icon(Icons.chat_bubble_outline),
                ),
                selectedIcon: const Icon(Icons.chat_bubble),
                label: 'Chats',
              ),
              NavigationDestination(
                icon: Badge(
                  isLabelVisible: pending > 0,
                  label: Text('$pending'),
                  child: const Icon(Icons.gavel_outlined),
                ),
                selectedIcon: const Icon(Icons.gavel),
                label: 'Approvals',
              ),
              const NavigationDestination(
                icon: Icon(Icons.schedule_outlined),
                selectedIcon: Icon(Icons.schedule),
                label: 'Workflows',
              ),
              const NavigationDestination(
                icon: Icon(Icons.smart_toy_outlined),
                selectedIcon: Icon(Icons.smart_toy),
                label: 'Bots',
              ),
              const NavigationDestination(
                icon: Icon(Icons.settings_outlined),
                selectedIcon: Icon(Icons.settings),
                label: 'Settings',
              ),
            ],
          ),
        );
      },
    );
  }
}

/// Shown when sign-in worked but the phone would not store the token.
class _SaveFailedBanner extends StatelessWidget {
  const _SaveFailedBanner({required this.session});

  final OwnerSession session;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      key: const Key('save-failed-banner'),
      color: scheme.tertiaryContainer,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
        child: Row(
          children: [
            Icon(Icons.warning_amber_rounded, color: scheme.onTertiaryContainer),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                'Signed in for this session only. This phone would not save '
                'your sign-in, so you will need to sign in again after the '
                'app restarts.',
                style: TextStyle(color: scheme.onTertiaryContainer),
              ),
            ),
            TextButton(
              onPressed: session.retrySave,
              child: const Text('Try saving again'),
            ),
          ],
        ),
      ),
    );
  }
}
