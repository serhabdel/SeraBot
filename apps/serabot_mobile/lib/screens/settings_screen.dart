import 'package:material_ui/material_ui.dart';
import 'package:package_info_plus/package_info_plus.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../models.dart';
import '../session.dart';
import '../theme.dart';
import '../status.dart';
import '../update_flow.dart';
import '../widgets.dart';
import 'alerts_screen.dart';
import 'costs_screen.dart';
import 'jobs_screen.dart';
import 'mail_screen.dart';
import 'usage_insights_screen.dart';

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
  List<AiProviderSummary>? _providers;
  String? _providersError;
  final Set<String> _refreshingQuotas = {};
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
      _providers = null;
      _providersError = null;
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
      _providers = await c.providers();
      _providersError = null;
    } on ApiException catch (e) {
      _providersError = e.messageForUser();
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

  Future<void> _refreshQuota(String providerId, String accountId) async {
    final key = '$providerId/$accountId';
    if (_refreshingQuotas.contains(key)) return;
    setState(() => _refreshingQuotas.add(key));
    try {
      final next = await widget.app.client
          .providerAccountQuota(providerId, accountId, refresh: true);
      if (!mounted) return;
      if (next != null && _providers != null) {
        setState(() {
          _providers = [
            for (final p in _providers!)
              if (p.id == providerId)
                AiProviderSummary(
                  id: p.id,
                  kind: p.kind,
                  auth: p.auth,
                  status: p.status,
                  health: p.health,
                  accounts: [
                    for (final a in p.accounts)
                      if (a.id == accountId) a.copyWithQuota(next) else a,
                  ],
                )
              else
                p,
          ];
        });
      }
    } on ApiException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    } finally {
      if (mounted) {
        setState(() => _refreshingQuotas.remove(key));
      }
    }
  }

  Future<void> _loadVersion() async {
    try {
      final v =
          await (widget.versionLoader ??
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
              child: Text(
                (user?.label.isNotEmpty ?? false)
                    ? user!.label.characters.first.toUpperCase()
                    : '?',
              ),
            ),
            title: Text(user?.label ?? 'Signed in'),
            subtitle: Text(
              [
                if (user != null && user.email.isNotEmpty) user.email,
                if (user != null && user.role.isNotEmpty) user.role,
              ].join(' \u00b7 '),
            ),
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
                  ButtonSegment(
                    value: ThemeMode.light,
                    label: Text('Light'),
                    icon: Icon(Icons.light_mode_outlined),
                  ),
                  ButtonSegment(
                    value: ThemeMode.dark,
                    label: Text('Dark'),
                    icon: Icon(Icons.dark_mode_outlined),
                  ),
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
          _section(context, 'AI Providers & Quotas'),
          ..._providerQuotaTiles(context),
          const Divider(),
          _section(context, 'Mail'),
          ListTile(
            key: const Key('mail-settings'),
            leading: const Icon(Icons.mail_outlined),
            title: const Text('Mail (SMTP / IMAP)'),
            subtitle: const Text(
              'Hostinger or any mailbox: connect, test and disconnect',
            ),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => MailScreen(app: widget.app),
              ),
            ),
          ),
          const Divider(),
          if (_usage?.quota.enforced ?? false) ...[
            _section(context, 'Quota'),
            _quotaTile(context, _usage!.quota),
            const Divider(),
          ],
          _section(context, 'Usage, last 30 days'),
          ..._usageTiles(context),
          ListTile(
            key: const Key('usage-insights'),
            leading: const Icon(Icons.insights_outlined),
            title: const Text('Usage insights'),
            subtitle: const Text(
              'Channels, the PC tunnel, model speed and server load',
            ),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => UsageInsightsScreen(app: widget.app),
              ),
            ),
          ),
          ListTile(
            key: const Key('usage-costs'),
            leading: const Icon(Icons.payments_outlined),
            title: const Text('Costs and budget'),
            subtitle: const Text('Estimated cost this month, and plans vs API'),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => CostsScreen(app: widget.app),
              ),
            ),
          ),
          const Divider(),
          _section(context, 'Alerts and jobs'),
          ListTile(
            key: const Key('alerts-settings'),
            leading: const Icon(Icons.notifications_outlined),
            title: const Text('Alerts and delivery'),
            subtitle: const Text('Telegram, WhatsApp, digests and the brief'),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => AlertsScreen(app: widget.app),
              ),
            ),
          ),
          ListTile(
            key: const Key('job-search'),
            leading: const Icon(Icons.work_outline),
            title: const Text('Job search'),
            subtitle: const Text(
              'Your profile, sources, applications and matches',
            ),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => JobsScreen(app: widget.app),
              ),
            ),
          ),
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
      StatusLevel.atLimit => 'You reached today\'s task limit. New bot turns wait until UTC midnight.',
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
          Text(
            'Tasks today (UTC): ${q.used} of ${q.limit}',
            style: Theme.of(context).textTheme.bodySmall,
          ),
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
          subtitle: Text(
            c.account != null && c.isConnected
                ? '${c.statusLabel} \u00b7 ${c.account}'
                : (c.needsDesktop ? 'Connect on desktop' : c.statusLabel),
          ),
        ),
      const Padding(
        padding: EdgeInsets.fromLTRB(16, 4, 16, 8),
        child: Text(
          'Connecting an account needs the sign-in page to return to '
          'your PC. Do it in the desktop app; it shows here afterwards.',
        ),
      ),
    ];
  }

  List<Widget> _providerQuotaTiles(BuildContext context) {
    if (_providersError != null) {
      return [_errorTile(_providersError!)];
    }
    final list = _providers;
    if (list == null) {
      return [const ListTile(title: Text('Loading\u2026'))];
    }
    final items = <({AiProviderSummary provider, ProviderAccountSummary account})>[
      for (final p in list)
        for (final a in p.accounts) (provider: p, account: a),
    ];
    if (items.isEmpty) {
      return [
        const ListTile(
          key: Key('provider-quotas-empty'),
          title: Text('No AI provider accounts connected.'),
        ),
      ];
    }
    final scheme = Theme.of(context).colorScheme;
    final textTheme = Theme.of(context).textTheme;
    return [
      for (final item in items)
        Padding(
          key: Key('provider-quota-${item.provider.id}-${item.account.id}'),
          padding: const EdgeInsets.fromLTRB(16, 6, 8, 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          item.account.label.isNotEmpty
                              ? '${item.provider.id} \u00b7 ${item.account.label}'
                              : item.provider.id,
                          style: textTheme.bodyLarge
                              ?.copyWith(fontWeight: FontWeight.w600),
                        ),
                        if (item.account.email != null ||
                            item.account.plan != null ||
                            (item.account.status.isNotEmpty &&
                                item.account.status != 'ok'))
                          Text(
                            [
                              if (item.account.email != null)
                                item.account.email!,
                              if (item.account.plan != null) item.account.plan!,
                              if (item.account.status.isNotEmpty &&
                                  item.account.status != 'ok')
                                item.account.status,
                            ].join(' \u00b7 '),
                            style: textTheme.bodySmall
                                ?.copyWith(color: scheme.onSurfaceVariant),
                          ),
                      ],
                    ),
                  ),
                  IconButton(
                    tooltip: 'Refresh quota',
                    onPressed: _refreshingQuotas
                            .contains('${item.provider.id}/${item.account.id}')
                        ? null
                        : () =>
                            _refreshQuota(item.provider.id, item.account.id),
                    icon: _refreshingQuotas
                            .contains('${item.provider.id}/${item.account.id}')
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Icon(Icons.refresh, size: 20),
                  ),
                ],
              ),
              const SizedBox(height: 4),
              ..._quotaWindowWidgets(context, item.account.quota),
            ],
          ),
        ),
    ];
  }

  List<Widget> _quotaWindowWidgets(
    BuildContext context,
    ProviderQuotaSnapshot? quota,
  ) {
    final scheme = Theme.of(context).colorScheme;
    final textTheme = Theme.of(context).textTheme;
    if (quota == null) {
      return [
        Text(
          'Quota unavailable',
          style: textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
        ),
      ];
    }
    if (quota.state == 'inactive') {
      return [
        Text(
          'Subscription inactive',
          style: textTheme.bodySmall?.copyWith(color: scheme.error),
        ),
      ];
    }
    if (quota.state == 'payment_required') {
      return [
        Text(
          'Payment required',
          style: textTheme.bodySmall?.copyWith(color: scheme.error),
        ),
      ];
    }
    if (quota.windows.isEmpty) {
      return [
        Text(
          'Quota unavailable',
          style: textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
        ),
      ];
    }
    return [
      for (final w in quota.windows) ...[
        Row(
          children: [
            Expanded(
              child: Text(
                w.label.isNotEmpty ? w.label : w.scope,
                style: textTheme.bodySmall,
              ),
            ),
            Text(
              '${w.remainingPercent}% left'
              '${w.resetsAt != null && w.resetsAt!.isNotEmpty ? ' \u00b7 Resets ${shortTime(w.resetsAt!)}' : ''}',
              style: textTheme.bodySmall
                  ?.copyWith(color: scheme.onSurfaceVariant),
            ),
          ],
        ),
        const SizedBox(height: 4),
        LinearProgressIndicator(
          value: w.remainingFraction,
          minHeight: 5,
          borderRadius: BorderRadius.circular(3),
        ),
        const SizedBox(height: 6),
      ],
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
      RegExp(r'\B(?=(\d{3})+(?!\d))'),
      (_) => ',',
    );
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
