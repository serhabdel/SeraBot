import 'package:connectivity_plus/connectivity_plus.dart';
import 'package:material_ui/material_ui.dart';

import '../app_state.dart';
import '../device.dart';
import '../session.dart';
import '../theme.dart';
import 'bots_screen.dart';
import 'chats_screen.dart';
import 'settings_screen.dart';
import 'tasks_screen.dart';
import 'workflows_screen.dart';

/// Signed-in shell: Chats, Tasks, Workflows, Bots, Settings. It owns the
/// [AppState] so the conversation list, unread counts, approvals, and event
/// stream live as long as the user is signed in.
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
    _app = AppState(
      widget.session.client,
      // Plugins are not available in widget tests, which also run without
      // the live stream.
      networkChanges: widget.startEvents ? _networkChanges() : null,
    );
    _device = widget.device ?? PlatformDeviceActions();
    _app.start(events: widget.startEvents);
  }

  /// Fires when the device comes back online or switches network.
  Stream<void> _networkChanges() {
    return Connectivity()
        .onConnectivityChanged
        .where((results) => !results.contains(ConnectivityResult.none))
        .map((_) {});
  }

  @override
  void dispose() {
    _app.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListenableBuilder(
      listenable: _app,
      builder: (context, _) {
        final unread = _app.unread;
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
                          TasksScreen(app: _app),
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
          bottomNavigationBar: NavigationBarTheme(
            data: NavigationBarThemeData(
              height: 70,
              indicatorColor: scheme.primaryContainer,
              indicatorShape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(16),
              ),
              labelTextStyle: WidgetStateProperty.resolveWith((states) {
                final selected = states.contains(WidgetState.selected);
                return Theme.of(context).textTheme.labelSmall?.copyWith(
                      fontWeight: selected ? FontWeight.w700 : FontWeight.w500,
                      color:
                          selected ? scheme.primary : scheme.onSurfaceVariant,
                    );
              }),
            ),
            child: NavigationBar(
              selectedIndex: _index,
              onDestinationSelected: (i) => setState(() => _index = i),
              labelBehavior: NavigationDestinationLabelBehavior.alwaysShow,
              destinations: [
                NavigationDestination(
                  icon: Badge(
                    isLabelVisible: unread > 0,
                    backgroundColor: scheme.primary,
                    textColor: scheme.onPrimary,
                    label: Text(unread > 99 ? '99+' : '$unread'),
                    child: const _NavVectorIcon(
                      glyph: _NavGlyph.chats,
                      selected: false,
                    ),
                  ),
                  selectedIcon: Badge(
                    isLabelVisible: unread > 0,
                    backgroundColor: scheme.primary,
                    textColor: scheme.onPrimary,
                    label: Text(unread > 99 ? '99+' : '$unread'),
                    child: const _NavVectorIcon(
                      glyph: _NavGlyph.chats,
                      selected: true,
                    ),
                  ),
                  label: 'Chats',
                ),
                const NavigationDestination(
                  icon: _NavVectorIcon(glyph: _NavGlyph.tasks, selected: false),
                  selectedIcon:
                      _NavVectorIcon(glyph: _NavGlyph.tasks, selected: true),
                  label: 'Tasks',
                ),
                const NavigationDestination(
                  icon: _NavVectorIcon(
                      glyph: _NavGlyph.workflows, selected: false),
                  selectedIcon: _NavVectorIcon(
                      glyph: _NavGlyph.workflows, selected: true),
                  label: 'Workflows',
                ),
                const NavigationDestination(
                  icon: _NavVectorIcon(glyph: _NavGlyph.bots, selected: false),
                  selectedIcon:
                      _NavVectorIcon(glyph: _NavGlyph.bots, selected: true),
                  label: 'Bots',
                ),
                const NavigationDestination(
                  icon: _NavVectorIcon(
                      glyph: _NavGlyph.settings, selected: false),
                  selectedIcon: _NavVectorIcon(
                      glyph: _NavGlyph.settings, selected: true),
                  label: 'Settings',
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

enum _NavGlyph { chats, tasks, workflows, bots, settings }

/// Crisp custom-drawn 24x24 vector icons for the 5 bottom navigation tabs.
class _NavVectorIcon extends StatelessWidget {
  const _NavVectorIcon({required this.glyph, required this.selected});

  final _NavGlyph glyph;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color =
        selected ? scheme.onPrimaryContainer : scheme.onSurfaceVariant;
    final accent = scheme.primary;
    return SizedBox(
      width: 24,
      height: 24,
      child: CustomPaint(
        painter: _NavGlyphPainter(
          glyph: glyph,
          selected: selected,
          color: color,
          accent: accent,
        ),
      ),
    );
  }
}

class _NavGlyphPainter extends CustomPainter {
  const _NavGlyphPainter({
    required this.glyph,
    required this.selected,
    required this.color,
    required this.accent,
  });

  final _NavGlyph glyph;
  final bool selected;
  final Color color;
  final Color accent;

  @override
  void paint(Canvas canvas, Size size) {
    final s = size.width / 24.0;
    final stroke = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.85 * s
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round;
    final fill = Paint()
      ..color = selected ? accent.withValues(alpha: 0.22) : Colors.transparent
      ..style = PaintingStyle.fill;

    switch (glyph) {
      case _NavGlyph.chats:
        final bubble = Path()
          ..moveTo(4 * s, 6.5 * s)
          ..quadraticBezierTo(4 * s, 4 * s, 6.5 * s, 4 * s)
          ..lineTo(17.5 * s, 4 * s)
          ..quadraticBezierTo(20 * s, 4 * s, 20 * s, 6.5 * s)
          ..lineTo(20 * s, 14.5 * s)
          ..quadraticBezierTo(20 * s, 17 * s, 17.5 * s, 17 * s)
          ..lineTo(10 * s, 17 * s)
          ..lineTo(5.8 * s, 20.2 * s)
          ..lineTo(6.3 * s, 17 * s)
          ..quadraticBezierTo(4 * s, 17 * s, 4 * s, 14.5 * s)
          ..close();
        if (selected) canvas.drawPath(bubble, fill);
        canvas.drawPath(bubble, stroke);
        canvas.drawLine(
            Offset(8.2 * s, 9.2 * s), Offset(15.8 * s, 9.2 * s), stroke);
        canvas.drawLine(
            Offset(8.2 * s, 12.6 * s), Offset(13.2 * s, 12.6 * s), stroke);

      case _NavGlyph.tasks:
        final card = RRect.fromRectAndRadius(
          Rect.fromLTWH(4 * s, 4 * s, 16 * s, 16 * s),
          Radius.circular(4 * s),
        );
        if (selected) canvas.drawRRect(card, fill);
        canvas.drawRRect(card, stroke);
        final check = Path()
          ..moveTo(7.6 * s, 10.2 * s)
          ..lineTo(9.4 * s, 12 * s)
          ..lineTo(12.2 * s, 8.6 * s);
        canvas.drawPath(check, stroke);
        canvas.drawLine(
            Offset(14 * s, 10.4 * s), Offset(16.8 * s, 10.4 * s), stroke);
        canvas.drawLine(
            Offset(7.8 * s, 15.2 * s), Offset(16.8 * s, 15.2 * s), stroke);

      case _NavGlyph.workflows:
        final topNode = RRect.fromRectAndRadius(
          Rect.fromLTWH(4 * s, 4.5 * s, 6 * s, 6 * s),
          Radius.circular(2 * s),
        );
        final botNode = RRect.fromRectAndRadius(
          Rect.fromLTWH(14 * s, 13.5 * s, 6 * s, 6 * s),
          Radius.circular(2 * s),
        );
        if (selected) {
          canvas.drawRRect(topNode, fill);
          canvas.drawRRect(botNode, fill);
        }
        canvas.drawRRect(topNode, stroke);
        canvas.drawRRect(botNode, stroke);
        final flow = Path()
          ..moveTo(10 * s, 7.5 * s)
          ..cubicTo(15.5 * s, 7.5 * s, 8.5 * s, 16.5 * s, 14 * s, 16.5 * s);
        canvas.drawPath(flow, stroke);
        canvas.drawCircle(
            Offset(17 * s, 7.5 * s),
            2 * s,
            Paint()
              ..color = selected ? accent : color
              ..style = selected ? PaintingStyle.fill : PaintingStyle.stroke
              ..strokeWidth = 1.6 * s);

      case _NavGlyph.bots:
        final head = RRect.fromRectAndRadius(
          Rect.fromLTWH(4.5 * s, 7 * s, 15 * s, 12 * s),
          Radius.circular(4 * s),
        );
        if (selected) canvas.drawRRect(head, fill);
        canvas.drawRRect(head, stroke);
        canvas.drawLine(Offset(12 * s, 4 * s), Offset(12 * s, 7 * s), stroke);
        canvas.drawCircle(Offset(12 * s, 3.6 * s), 1.1 * s,
            Paint()..color = selected ? accent : color);
        canvas.drawCircle(
            Offset(9.4 * s, 12.6 * s), 1.3 * s, Paint()..color = color);
        canvas.drawCircle(
            Offset(14.6 * s, 12.6 * s), 1.3 * s, Paint()..color = color);
        canvas.drawLine(
            Offset(9.6 * s, 15.8 * s), Offset(14.4 * s, 15.8 * s), stroke);

      case _NavGlyph.settings:
        final center = Offset(12 * s, 12 * s);
        if (selected) {
          canvas.drawCircle(center, 7.5 * s, fill);
        }
        canvas.drawCircle(center, 3.1 * s, stroke);
        for (var i = 0; i < 6; i++) {
          final angle = i * 3.14159265 / 3.0;
          final inner = Offset(
            center.dx + 6.4 * s * _cos(angle),
            center.dy + 6.4 * s * _sin(angle),
          );
          final outer = Offset(
            center.dx + 8.8 * s * _cos(angle),
            center.dy + 8.8 * s * _sin(angle),
          );
          canvas.drawLine(inner, outer, stroke);
        }
        canvas.drawCircle(center, 6.4 * s, stroke);
    }
  }

  double _cos(double r) {
    // Taylor-free exact lookup for 60-degree multiples.
    const vals = [1.0, 0.5, -0.5, -1.0, -0.5, 0.5];
    final idx = ((r / (3.14159265 / 3.0)).round()) % 6;
    return vals[idx];
  }

  double _sin(double r) {
    const vals = [0.0, 0.8660254, 0.8660254, 0.0, -0.8660254, -0.8660254];
    final idx = ((r / (3.14159265 / 3.0)).round()) % 6;
    return vals[idx];
  }

  @override
  bool shouldRepaint(_NavGlyphPainter old) =>
      old.glyph != glyph ||
      old.selected != selected ||
      old.color != color ||
      old.accent != accent;
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
