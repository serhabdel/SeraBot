import 'package:material_ui/material_ui.dart';

import '../alerts_models.dart';
import '../api_client.dart';
import '../app_state.dart';
import '../widgets.dart';

/// Where alerts reach the member and what they get: Telegram, one WhatsApp
/// chat, file delivery, the weekly digest and (0.70) the morning brief and
/// email triage. Mirrors the desktop Alerts settings. Only the member's own
/// settings; nothing here reads or changes another member's chats.
class AlertsScreen extends StatefulWidget {
  const AlertsScreen({super.key, required this.app});

  final AppState app;

  @override
  State<AlertsScreen> createState() => _AlertsScreenState();
}

class _AlertsScreenState extends State<AlertsScreen> {
  AlertsView? _view;
  String? _error;
  bool _loading = true;
  bool _saving = false;

  /// False once the server answers 404 for the brief preview (older server).
  bool _briefSupported = true;

  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final view = await widget.app.client.alerts();
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _view = view;
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  Future<void> _save(AlertsUpdate next) async {
    setState(() => _saving = true);
    try {
      final view = await widget.app.client.saveAlerts(next);
      if (!mounted) {
        return;
      }
      setState(() {
        _view = view;
        _saving = false;
      });
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _saving = false);
      _snack(e.messageForUser());
    }
  }

  void _snack(String message) {
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _sendDigestNow() async {
    setState(() => _saving = true);
    try {
      final preview = await widget.app.client.sendDigestNow();
      if (!mounted) {
        return;
      }
      setState(() => _saving = false);
      await _showText(
        'Your weekly digest',
        preview.text.isEmpty ? 'The digest is empty this week.' : preview.text,
        footer: preview.delivered
            ? 'Sent to your alert chat.'
            : 'Preview only. No alert chat is linked, so nothing was sent.',
      );
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _saving = false);
      _snack(e.messageForUser());
    }
  }

  Future<void> _sendBriefNow() async {
    try {
      final text = await widget.app.client.briefPreview();
      if (!mounted) {
        return;
      }
      if (text == null) {
        setState(() => _briefSupported = false);
        _snack('Your server does not have the brief yet. Update it to 0.70.');
        return;
      }
      await _showText(
        'Your brief',
        text.isEmpty ? 'Nothing to brief yet.' : text,
      );
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      _snack(e.messageForUser());
    }
  }

  Future<void> _showText(String title, String text, {String? footer}) {
    return showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SelectableText(text),
              if (footer != null) ...[
                const SizedBox(height: 12),
                Text(footer, style: Theme.of(context).textTheme.bodySmall),
              ],
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final view = _view;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Alerts and delivery'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          if (_error != null) ...[
            ErrorBanner(_error!, onRetry: _load),
            const SizedBox(height: 12),
          ],
          if (view == null && _error == null)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 48),
              child: Center(child: CircularProgressIndicator()),
            )
          else if (view != null) ...[
            if (!view.enabled)
              const Padding(
                padding: EdgeInsets.only(bottom: 12),
                child: Text('The owner has turned alerts off on this server.'),
              ),
            _sectionTitle(context, 'Where alerts go'),
            SwitchListTile(
              key: const Key('alerts-telegram'),
              title: const Text('Telegram'),
              subtitle: Text(
                view.telegramLinked
                    ? 'Send alerts to my Telegram chat'
                    : 'Telegram is not linked to your account yet',
              ),
              value: view.telegram,
              onChanged: _saving || (!view.telegramLinked && !view.telegram)
                  ? null
                  : (on) => _save(view.toUpdate().copyWith(telegram: on)),
            ),
            _whatsappSection(context, view),
            _sectionTitle(context, 'Files and digests'),
            SwitchListTile(
              key: const Key('alerts-auto-files'),
              title: const Text('Send files to my own chats without asking'),
              value: view.autoSendFiles,
              onChanged: _saving
                  ? null
                  : (on) => _save(view.toUpdate().copyWith(autoSendFiles: on)),
            ),
            SwitchListTile(
              key: const Key('alerts-weekly-digest'),
              title: const Text('Weekly digest'),
              subtitle: Text(
                view.digestEnabled
                    ? 'A short summary once a week'
                    : 'The owner has the weekly digest turned off',
              ),
              value: view.weeklyDigest,
              onChanged: _saving
                  ? null
                  : (on) => _save(view.toUpdate().copyWith(weeklyDigest: on)),
            ),
            if (view.morningBrief != null)
              SwitchListTile(
                key: const Key('alerts-morning-brief'),
                title: const Text('Morning brief'),
                subtitle: view.briefHourUtc == null
                    ? null
                    : Text(
                        'Around ${view.briefHourUtc!.clamp(0, 23).toString().padLeft(2, '0')}:00 UTC',
                      ),
                value: view.morningBrief!,
                onChanged: _saving
                    ? null
                    : (on) => _save(view.toUpdate().copyWith(morningBrief: on)),
              ),
            if (view.emailTriage != null)
              SwitchListTile(
                key: const Key('alerts-email-triage'),
                title: const Text('Email triage'),
                subtitle: const Text('Sort new mail by what needs an answer'),
                value: view.emailTriage!,
                onChanged: _saving
                    ? null
                    : (on) => _save(view.toUpdate().copyWith(emailTriage: on)),
              ),
            const SizedBox(height: 16),
            _sectionTitle(context, 'Send now'),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                FilledButton.tonal(
                  key: const Key('alerts-digest-now'),
                  onPressed: _saving ? null : _sendDigestNow,
                  child: const Text('Send me the digest now'),
                ),
                if (_briefSupported)
                  OutlinedButton(
                    key: const Key('alerts-brief-now'),
                    onPressed: _saving ? null : _sendBriefNow,
                    child: const Text('Send me my brief now'),
                  ),
              ],
            ),
            const SizedBox(height: 16),
            _sectionTitle(context, 'Recent alerts'),
            if (view.recent.isEmpty)
              const ListTile(title: Text('No alerts sent yet.'))
            else
              for (final alert in view.recent)
                ListTile(
                  dense: true,
                  title: Text(
                    alert.text.isEmpty ? alert.kind : alert.text,
                    maxLines: 3,
                    overflow: TextOverflow.ellipsis,
                  ),
                  subtitle: Text(
                    [
                      _shortTime(alert.at),
                      if (alert.kind.isNotEmpty) alert.kind,
                    ].join(' · '),
                  ),
                ),
            const SizedBox(height: 16),
            _sectionTitle(context, 'Alert thresholds (set by the owner)'),
            ListTile(
              dense: true,
              title: const Text('Disk and memory'),
              trailing: Text(
                'disk ${view.thresholds.diskPercent}% · '
                'memory ${view.thresholds.memoryPercent}%',
              ),
            ),
            ListTile(
              dense: true,
              title: const Text('Failovers and repeats'),
              trailing: Text(
                '${view.thresholds.failoversIn15Min} in 15 min · '
                'every ${view.thresholds.cooldownMins} min',
              ),
            ),
          ],
          const SizedBox(height: 24),
        ],
      ),
    );
  }

  Widget _whatsappSection(BuildContext context, AlertsView view) {
    final current = view.whatsappConversationId;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _sectionTitle(context, 'WhatsApp chat'),
        if (view.whatsappChoices.isEmpty)
          const ListTile(
            dense: true,
            title: Text('No WhatsApp chats are allowed for you yet.'),
          ),
        ListTile(
          key: const Key('alerts-wa-off'),
          title: const Text('Off'),
          trailing: current == null ? const Icon(Icons.check) : null,
          onTap: _saving || current == null
              ? null
              : () => _save(
                  view.toUpdate().copyWith(whatsappConversationId: null),
                ),
        ),
        for (final choice in view.whatsappChoices)
          ListTile(
            key: Key('alerts-wa-${choice.conversationId}'),
            title: Text(choice.label.isEmpty ? 'WhatsApp chat' : choice.label),
            trailing: current == choice.conversationId
                ? const Icon(Icons.check)
                : null,
            onTap: _saving || current == choice.conversationId
                ? null
                : () => _save(
                    view.toUpdate().copyWith(
                      whatsappConversationId: choice.conversationId,
                    ),
                  ),
          ),
      ],
    );
  }

  Widget _sectionTitle(BuildContext context, String text) => Padding(
    padding: const EdgeInsets.fromLTRB(0, 12, 0, 4),
    child: Text(text, style: Theme.of(context).textTheme.titleSmall),
  );

  /// "2026-11-20T09:05:00Z" becomes "2026-11-20 09:05".
  String _shortTime(String at) {
    final spaced = at.replaceFirst('T', ' ');
    return spaced.length > 16 ? spaced.substring(0, 16) : spaced;
  }
}
