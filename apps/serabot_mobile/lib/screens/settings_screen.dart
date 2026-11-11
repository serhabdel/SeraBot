import 'package:flutter/material.dart';
import 'package:package_info_plus/package_info_plus.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../session.dart';
import '../theme.dart';
import '../status.dart';
import '../update_flow.dart';
import '../widgets.dart';

/// Account, theme, read-only connector status, usage summary and About.
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.session,
    required this.app,
    required this.theme,
    this.versionLoader,
    this.onCheckUpdates,
  });

  final OwnerSession session;
  final AppState app;
  final ThemeController theme;

  /// Test hook; defaults to the installed app version.
  final Future<String> Function()? versionLoader;

  /// Test hook; defaults to the GitHub release check.
  final void Function(BuildContext)? onCheckUpdates;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  List<ConnectorStatus>? _connectors;
  String? _connectorsError;
  UsageReport? _usage;
  String? _usageError;
  String _version = '';

  @override
  void initState() {
    super.initState();
    _loadAll();
  }

  void _reload() {
    setState(() {
      _connectors = null;
      _connectorsError = null;
      _usage = null;
      _usageError = null;
    });
    _loadAll();
  }

  Future<void> _loadAll() async {
    final c = widget.app.client;
    _loadVersion();
    try {
      _connectors = await c.connectors();
      _connectorsError = null;
    } on ApiException catch (e) {
      _connectorsError = e.messageForUser();
    }
    try {
      _usage = await c.usage();
      _usageError = null;
    } on ApiException catch (e) {
      _usageError = e.messageForUser();
    }
    if (mounted) {
      setState(() {});
    }
  }

  Future<void> _loadVersion() async {
    try {
      final v = await (widget.versionLoader ??
          () async => (await PackageInfo.fromPlatform()).version)();
      if (mounted) {
        setState(() => _version = v);
      }
    } catch (_) {
      // The version label is cosmetic.
    }
  }

  @override
  Widget build(BuildContext context) {
    final user = widget.session.user;
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      appBar: AppBar(title: const Text('Settings')),
      body: ListView(
        children: [
          ListTile(
            leading: CircleAvatar(
              backgroundColor: scheme.primaryContainer,
              child: Text((user?.label.isNotEmpty ?? false)
                  ? user!.label.characters.first.toUpperCase()
                  : '?'),
            ),
            title: Text(user?.label ?? 'Signed in'),
            subtitle: Text([
              if (user != null && user.email.isNotEmpty) user.email,
              if (user != null && user.role.isNotEmpty) user.role,
            ].join(' \u00b7 ')),
          ),
          ListTile(
            leading: const Icon(Icons.dns_outlined),
            title: const Text('Server'),
            subtitle: Text(widget.session.endpoint ?? ''),
            trailing: Text(widget.session.serverVersion ?? ''),
          ),
          const Divider(),
          _section(context, 'Theme'),
          ListenableBuilder(
            listenable: widget.theme,
            builder: (context, _) => Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: SegmentedButton<ThemeMode>(
                key: const Key('theme-toggle'),
                segments: const [
                  ButtonSegment(value: ThemeMode.light, label: Text('Light'), icon: Icon(Icons.light_mode_outlined)),
                  ButtonSegment(value: ThemeMode.dark, label: Text('Dark'), icon: Icon(Icons.dark_mode_outlined)),
                  ButtonSegment(value: ThemeMode.system, label: Text('System')),
                ],
                selected: {widget.theme.mode},
                onSelectionChanged: (s) => widget.theme.set(s.first),
              ),
            ),
          ),
          const Divider(),
          _section(context, 'Connectors'),
          ..._connectorTiles(context),
          const Divider(),
          if (_usage?.quota.enforced ?? false) ...[
            _section(context, 'Quota'),
            _quotaTile(context, _usage!.quota),
            const Divider(),
          ],
          _section(context, 'Usage, last 30 days'),
          ..._usageTiles(context),
          const Divider(),
          _section(context, 'About'),
          ListTile(
            leading: const Icon(Icons.info_outline),
            title: const Text('SeraBot for Android'),
            subtitle: Text(_version.isEmpty ? '' : 'Version $_version'),
          ),
          ListTile(
            key: const Key('check-updates'),
            leading: const Icon(Icons.system_update),
            title: const Text('Check for updates'),
            onTap: () => (widget.onCheckUpdates ?? checkForUpdates)(context),
          ),
          ListTile(
            key: const Key('sign-out'),
            leading: const Icon(Icons.logout),
            title: const Text('Sign out'),
            onTap: _confirmSignOut,
          ),
          const SizedBox(height: 24),
        ],
      ),
    );
  }

  Future<void> _confirmSignOut() async {
    final ok = await confirmAction(
      context,
      title: 'Sign out?',
      message: 'You will need to sign in again to use SeraBot on this phone.',
      confirmLabel: 'Sign out',
    );
    if (ok) {
      await widget.session.signOut();
    }
  }

  Widget _errorTile(String message) => Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
        child: ErrorBanner(message, onRetry: _reload),
      );

  /// Daily task budget: a word, an icon and a bar, never colour alone.
  Widget _quotaTile(BuildContext context, UsageQuota q) {
    final level = statusLevelFromFlags(
      nearLimit: q.nearLimit,
      atLimit: q.atLimit,
    );
    final percent = quotaPercent(q.used, q.limit);
    final note = switch (level) {
      StatusLevel.atLimit =>
        'You reached today\'s task limit. New bot turns wait until UTC midnight.',
      StatusLevel.near => 'You are close to today\'s task limit.',
      StatusLevel.ok =>
        'Queued and completed tasks count toward this UTC-day budget.',
    };
    return Padding(
      key: const Key('quota-tile'),
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              const Expanded(child: Text('Daily task quota')),
              StatusChip(level: level),
            ],
          ),
          const SizedBox(height: 8),
          Semantics(
            label: 'Daily task quota, $percent percent used',
            child: LinearProgressIndicator(
              value: percent / 100,
              minHeight: 6,
              borderRadius: BorderRadius.circular(3),
            ),
          ),
          const SizedBox(height: 6),
          Text('Tasks today (UTC): ${q.used} of ${q.limit}',
              style: Theme.of(context).textTheme.bodySmall),
          Text(note, style: Theme.of(context).textTheme.bodySmall),
        ],
      ),
    );
  }

  Widget _section(BuildContext context, String t) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
        child: Text(t, style: Theme.of(context).textTheme.titleSmall),
      );

  List<Widget> _connectorTiles(BuildContext context) {
    if (_connectorsError != null) {
      return [_errorTile(_connectorsError!)];
    }
    final list = _connectors;
    if (list == null) {
      return [const ListTile(title: Text('Loading\u2026'))];
    }
    if (list.isEmpty) {
      return [const ListTile(title: Text('No connectors configured.'))];
    }
    return [
      for (final c in list)
        ListTile(
          dense: true,
          leading: Icon(
            c.isConnected ? Icons.check_circle : Icons.link_off,
            color: c.isConnected ? Theme.of(context).colorScheme.primary : null,
          ),
          title: Text(c.id),
          subtitle: Text(c.account != null && c.isConnected
              ? '${c.statusLabel} \u00b7 ${c.account}'
              : (c.needsDesktop ? 'Connect on desktop' : c.statusLabel)),
        ),
      const Padding(
        padding: EdgeInsets.fromLTRB(16, 4, 16, 8),
        child: Text('Connecting an account needs the sign-in page to return to '
            'your PC. Do it in the desktop app; it shows here afterwards.'),
      ),
    ];
  }

  List<Widget> _usageTiles(BuildContext context) {
    if (_usageError != null) {
      return [_errorTile(_usageError!)];
    }
    final u = _usage;
    if (u == null) {
      return [const ListTile(title: Text('Loading\u2026'))];
    }
    String n(int v) => v.toString().replaceAllMapped(
        RegExp(r'\B(?=(\d{3})+(?!\d))'), (_) => ',');
    return [
      ListTile(
        dense: true,
        title: const Text('Model calls'),
        trailing: Text(n(u.totals.calls)),
      ),
      ListTile(
        dense: true,
        title: const Text('Tokens'),
        trailing: Text(n(u.totals.totalTokens)),
      ),
      for (final m in u.perModel.take(5))
        ListTile(
          dense: true,
          title: Text(m.model, overflow: TextOverflow.ellipsis),
          subtitle: Text(m.provider),
          trailing: Text(n(m.totals.totalTokens)),
        ),
    ];
  }
}
