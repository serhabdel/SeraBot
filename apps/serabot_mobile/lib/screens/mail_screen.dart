import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../mail_models.dart';
import '../widgets.dart';

/// Mail (SMTP / IMAP) connector card (0.74.2 desktop parity): connected /
/// not-connected badge, one-click Hostinger preset, editable SMTP/IMAP host,
/// port, security, username, masked write-only password and optional From
/// address, live per-side SMTP/IMAP test results after saving, and
/// Disconnect. Credentials live in the member's own secret store on the
/// server; the password is never returned by the API or shown again.
class MailScreen extends StatefulWidget {
  const MailScreen({super.key, required this.app});

  final AppState app;

  @override
  State<MailScreen> createState() => _MailScreenState();
}

class _MailScreenState extends State<MailScreen> {
  final _smtpHost = TextEditingController();
  final _smtpPort = TextEditingController(text: '587');
  final _imapHost = TextEditingController();
  final _imapPort = TextEditingController(text: '993');
  final _username = TextEditingController();
  final _password = TextEditingController();
  final _from = TextEditingController();
  final _account = TextEditingController();

  MailSecurity _smtpSecurity = MailSecurity.startTls;
  MailSecurity _imapSecurity = MailSecurity.ssl;

  MailStatus? _status;
  MailConnectResponse? _lastConnect;
  String? _error;
  bool _loading = true;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _smtpHost.dispose();
    _smtpPort.dispose();
    _imapHost.dispose();
    _imapPort.dispose();
    _username.dispose();
    _password.dispose();
    _from.dispose();
    _account.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final status = await widget.app.client.mailStatus();
      if (!mounted) return;
      setState(() {
        _status = status;
        _loading = false;
        if (status.smtpHost != null && _smtpHost.text.isEmpty) {
          _smtpHost.text = status.smtpHost!;
        }
        if (status.imapHost != null && _imapHost.text.isEmpty) {
          _imapHost.text = status.imapHost!;
        }
        if (status.username != null && _username.text.isEmpty) {
          _username.text = status.username!;
        }
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  void _applyHostinger() {
    setState(() {
      _smtpHost.text = 'smtp.hostinger.com';
      _smtpPort.text = '587';
      _smtpSecurity = MailSecurity.startTls;
      _imapHost.text = 'imap.hostinger.com';
      _imapPort.text = '993';
      _imapSecurity = MailSecurity.ssl;
    });
  }

  Future<void> _connect() async {
    final smtpPort = int.tryParse(_smtpPort.text.trim());
    final imapPort = int.tryParse(_imapPort.text.trim());
    if (smtpPort == null || imapPort == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Enter valid SMTP and IMAP ports.')),
      );
      return;
    }
    final req = MailConnectRequest(
      smtpHost: _smtpHost.text.trim(),
      smtpPort: smtpPort,
      smtpSecurity: _smtpSecurity,
      username: _username.text.trim(),
      password: _password.text,
      imapHost: _imapHost.text.trim(),
      imapPort: imapPort,
      imapSecurity: _imapSecurity,
      from: _from.text.trim().isEmpty ? null : _from.text.trim(),
      account: _account.text.trim().isEmpty ? null : _account.text.trim(),
    );
    setState(() {
      _busy = true;
      _error = null;
    });
    final messenger = ScaffoldMessenger.of(context);
    try {
      final resp = await widget.app.client.mailConnect(req);
      if (!mounted) return;
      setState(() {
        _lastConnect = resp;
        _busy = false;
        // Write-only: the server never returns the password, and the
        // phone stops holding it once the save succeeds.
        _password.clear();
      });
      // Re-fetch so the accounts list includes the newly connected mailbox.
      await _load();
      messenger.showSnackBar(
        SnackBar(
          content: Text(
            resp.connected
                ? 'Mail connected. Tests: ${_probeWord(resp.smtpTest)} SMTP, ${_probeWord(resp.imapTest)} IMAP.'
                : 'Mail saved, but a login test failed. Check the results below.',
          ),
        ),
      );
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = e.messageForUser();
      });
    }
  }

  String _probeWord(MailProbeOutcome p) => p.succeeded ? 'ok' : 'failed';

  Future<void> _disconnect() async {
    final ok = await confirmAction(
      context,
      title: 'Disconnect mail?',
      message:
          'Your saved SMTP/IMAP credentials are removed from the server. '
          'Mail tools stop working until you connect again.',
      confirmLabel: 'Disconnect',
    );
    if (!ok || !mounted) return;
    setState(() => _busy = true);
    final messenger = ScaffoldMessenger.of(context);
    try {
      final acct = _account.text.trim();
      await widget.app.client.mailDisconnect(
        account: acct.isEmpty ? null : acct,
      );
      if (!mounted) return;
      setState(() {
        _lastConnect = null;
        _password.clear();
        _busy = false;
      });
      await _load();
      messenger.showSnackBar(
        const SnackBar(content: Text('Mail disconnected')),
      );
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() => _busy = false);
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final connected = _status?.connected ?? false;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Mail (SMTP / IMAP)'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: _loading && _status == null && _error == null
          ? const Center(child: CircularProgressIndicator())
          : ListView(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 32),
              children: [
                if (_error != null) ...[
                  ErrorBanner(_error!, onRetry: _load),
                  const SizedBox(height: 12),
                ],
                Card(
                  key: const Key('mail-status'),
                  child: Padding(
                    padding: const EdgeInsets.all(16),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          children: [
                            Icon(
                              connected
                                  ? Icons.check_circle
                                  : Icons.link_off,
                              color: connected ? scheme.primary : null,
                            ),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                connected ? 'Connected' : 'Not connected',
                                style: Theme.of(context).textTheme.titleMedium,
                              ),
                            ),
                          ],
                        ),
                        if (connected) ...[
                          const SizedBox(height: 8),
                          if (_status!.username != null)
                            Text('Account: ${_status!.username}'),
                          if (_status!.smtpHost != null)
                            Text('SMTP: ${_status!.smtpHost}'),
                          if (_status!.imapHost != null)
                            Text('IMAP: ${_status!.imapHost}'),
                          if (_status!.accounts.length > 1)
                            Text(
                              'Mailboxes: ${_status!.accounts.map((a) => a.account.isEmpty ? 'default (${a.username})' : '${a.account} (${a.username})').join(', ')}',
                            ),
                        ] else
                          const SizedBox(height: 8),
                        if (!connected)
                          const Text(
                            'Connect a mailbox you control outright — for '
                            'example Hostinger — so SeraBot can list, read '
                            'and (with approval) send mail.',
                          ),
                      ],
                    ),
                  ),
                ),
                if (_lastConnect != null) ...[
                  const SizedBox(height: 12),
                  Card(
                    key: const Key('mail-tests'),
                    child: Padding(
                      padding: const EdgeInsets.all(16),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            'Live test results',
                            style: Theme.of(context).textTheme.titleSmall,
                          ),
                          const SizedBox(height: 8),
                          _probeTile(context, 'SMTP', _lastConnect!.smtpTest),
                          _probeTile(context, 'IMAP', _lastConnect!.imapTest),
                        ],
                      ),
                    ),
                  ),
                ],
                const SizedBox(height: 12),
                FilledButton.tonal(
                  key: const Key('mail-hostinger-preset'),
                  onPressed: _busy ? null : _applyHostinger,
                  child: const Text('Use Hostinger preset'),
                ),
                const SizedBox(height: 4),
                const Text(
                  'Preset: smtp.hostinger.com:587 STARTTLS, '
                  'imap.hostinger.com:993 SSL. Then enter your mailbox '
                  'username and password.',
                  style: TextStyle(fontSize: 13),
                ),
                const SizedBox(height: 16),
                Text('SMTP', style: Theme.of(context).textTheme.titleSmall),
                const SizedBox(height: 8),
                TextField(
                  key: const Key('mail-smtp-host'),
                  controller: _smtpHost,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'SMTP host',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 8),
                Row(
                  children: [
                    Expanded(
                      child: TextField(
                        key: const Key('mail-smtp-port'),
                        controller: _smtpPort,
                        keyboardType: TextInputType.number,
                        decoration: const InputDecoration(
                          labelText: 'Port',
                          border: OutlineInputBorder(),
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      flex: 2,
                      child: _securityField(
                        label: 'Security',
                        value: _smtpSecurity,
                        onChanged: (v) => setState(() {
                          if (v != null) _smtpSecurity = v;
                        }),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 16),
                Text('IMAP', style: Theme.of(context).textTheme.titleSmall),
                const SizedBox(height: 8),
                TextField(
                  key: const Key('mail-imap-host'),
                  controller: _imapHost,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'IMAP host',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 8),
                Row(
                  children: [
                    Expanded(
                      child: TextField(
                        key: const Key('mail-imap-port'),
                        controller: _imapPort,
                        keyboardType: TextInputType.number,
                        decoration: const InputDecoration(
                          labelText: 'Port',
                          border: OutlineInputBorder(),
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      flex: 2,
                      child: _securityField(
                        label: 'Security',
                        value: _imapSecurity,
                        onChanged: (v) => setState(() {
                          if (v != null) _imapSecurity = v;
                        }),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 16),
                TextField(
                  key: const Key('mail-account'),
                  controller: _account,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'Account name (optional — default mailbox when empty)',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 16),
                Text('Account', style: Theme.of(context).textTheme.titleSmall),
                const SizedBox(height: 8),
                TextField(
                  key: const Key('mail-username'),
                  controller: _username,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'Username / email address',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 8),
                SecretField(
                  key: const Key('mail-password'),
                  controller: _password,
                  label: 'Password',
                  hint: 'Write-only: never shown again after saving',
                ),
                const SizedBox(height: 8),
                TextField(
                  key: const Key('mail-from'),
                  controller: _from,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'From address (optional)',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 16),
                SizedBox(
                  width: double.infinity,
                  child: FilledButton(
                    key: const Key('mail-connect'),
                    onPressed: _busy ? null : _connect,
                    child: Text(connected ? 'Save and test again' : 'Connect'),
                  ),
                ),
                if (connected) ...[
                  const SizedBox(height: 8),
                  SizedBox(
                    width: double.infinity,
                    child: OutlinedButton(
                      key: const Key('mail-disconnect'),
                      onPressed: _busy ? null : _disconnect,
                      child: const Text('Disconnect'),
                    ),
                  ),
                ],
                const SizedBox(height: 12),
                const Text(
                  'Sending mail needs your approval each time. Plain '
                  '(non-TLS) connections are refused unless the host is '
                  'localhost.',
                  style: TextStyle(fontSize: 13),
                ),
              ],
            ),
    );
  }

  Widget _probeTile(BuildContext context, String label, MailProbeOutcome p) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(top: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            p.succeeded ? Icons.check_circle : Icons.error_outline,
            size: 20,
            color: p.succeeded ? scheme.primary : scheme.error,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              p.succeeded ? '$label: ok' : '$label: ${p.error}',
            ),
          ),
        ],
      ),
    );
  }

  Widget _securityField({
    required String label,
    required MailSecurity value,
    required ValueChanged<MailSecurity?> onChanged,
  }) {
    return DropdownButtonFormField<MailSecurity>(
      initialValue: value,
      decoration: InputDecoration(
        labelText: label,
        border: const OutlineInputBorder(),
      ),
      items: [
        for (final s in MailSecurity.values)
          DropdownMenuItem(value: s, child: Text(s.label)),
      ],
      onChanged: onChanged,
    );
  }
}
