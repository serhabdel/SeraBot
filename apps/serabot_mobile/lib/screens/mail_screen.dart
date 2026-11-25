import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../mail_models.dart';
import '../widgets.dart';

/// Mail (SMTP / IMAP) screen: interactive inbox/folders, server-side search,
/// star/read/delete, attachment Save to Library, Compose/Reply sheet, and the
/// SMTP/IMAP connection settings form.
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
  final _searchController = TextEditingController();

  MailSecurity _smtpSecurity = MailSecurity.startTls;
  MailSecurity _imapSecurity = MailSecurity.ssl;

  MailStatus? _status;
  MailConnectResponse? _lastConnect;
  String? _error;
  bool _loading = true;
  bool _busy = false;

  // Interactive mailbox state
  bool _showSettings = false;
  String? _selectedAccount;
  List<String> _folders = const ['INBOX'];
  String _selectedFolder = 'INBOX';
  List<MailMessageInfo> _messages = const [];
  bool _loadingMessages = false;
  String? _messagesError;
  String _activeSearch = '';

  bool get _isConnected =>
      (_status?.connected ?? false) || (_status?.accounts.isNotEmpty ?? false);

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
    _searchController.dispose();
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
        if (_selectedAccount == null && status.accounts.isNotEmpty) {
          final first = status.accounts.first.account;
          _selectedAccount = first.isEmpty ? null : first;
        }
      });
      if (status.connected || status.accounts.isNotEmpty) {
        await _loadMailbox();
      }
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  Future<void> _loadMailbox() async {
    setState(() {
      _loadingMessages = true;
      _messagesError = null;
    });
    try {
      final folders = await widget.app.client.mailFolders(
        account: _selectedAccount,
      );
      final normalizedFolders = folders.isEmpty ? const ['INBOX'] : folders;
      final folder = normalizedFolders.contains(_selectedFolder)
          ? _selectedFolder
          : normalizedFolders.first;
      final msgs = _activeSearch.isNotEmpty
          ? await widget.app.client.mailSearch(
              account: _selectedAccount,
              folder: folder,
              query: _activeSearch,
            )
          : await widget.app.client.mailMessages(
              account: _selectedAccount,
              folder: folder,
            );
      if (!mounted) return;
      setState(() {
        _folders = normalizedFolders;
        _selectedFolder = folder;
        _messages = msgs;
        _loadingMessages = false;
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _messagesError = e.messageForUser();
        _loadingMessages = false;
      });
    }
  }

  Future<void> _runSearch() async {
    final q = _searchController.text.trim();
    if (q.isEmpty) {
      await _clearSearch();
      return;
    }
    setState(() {
      _activeSearch = q;
      _loadingMessages = true;
      _messagesError = null;
    });
    try {
      final hits = await widget.app.client.mailSearch(
        account: _selectedAccount,
        folder: _selectedFolder,
        query: q,
      );
      if (!mounted) return;
      setState(() {
        _messages = hits;
        _loadingMessages = false;
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _messagesError = e.messageForUser();
        _loadingMessages = false;
      });
    }
  }

  Future<void> _clearSearch() async {
    _searchController.clear();
    if (_activeSearch.isEmpty && _messages.isNotEmpty) {
      setState(() {});
      return;
    }
    setState(() {
      _activeSearch = '';
    });
    await _loadMailbox();
  }

  Future<void> _toggleFlag(
    MailMessageInfo msg, {
    bool? seen,
    bool? flagged,
  }) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await widget.app.client.mailFlag(
        account: _selectedAccount,
        folder: _selectedFolder,
        uid: msg.uid,
        seen: seen,
        flagged: flagged,
      );
      if (!mounted) return;
      setState(() {
        _messages = [
          for (final m in _messages)
            if (m.uid == msg.uid)
              m.withUpdatedFlags(seen: seen, flagged: flagged)
            else
              m,
        ];
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _deleteMessage(MailMessageInfo msg) async {
    final ok = await confirmAction(
      context,
      title: 'Delete message?',
      message:
          'Permanently delete "${msg.subject.isEmpty ? '(No subject)' : msg.subject}"?',
      confirmLabel: 'Delete',
    );
    if (!ok || !mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await widget.app.client.mailDelete(
        account: _selectedAccount,
        folder: _selectedFolder,
        uid: msg.uid,
      );
      if (!mounted) return;
      setState(() {
        _messages = _messages.where((m) => m.uid != msg.uid).toList();
      });
      messenger.showSnackBar(
        const SnackBar(content: Text('Message deleted')),
      );
    } on ApiException catch (e) {
      if (!mounted) return;
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _openMessage(MailMessageInfo msg) async {
    final result = await Navigator.of(context).push<_DetailOutcome>(
      MaterialPageRoute(
        builder: (_) => _MailMessageDetailScreen(
          app: widget.app,
          account: _selectedAccount,
          folder: _selectedFolder,
          initial: msg,
        ),
      ),
    );
    if (!mounted || result == null) return;
    if (result.deleted) {
      setState(() {
        _messages = _messages.where((m) => m.uid != msg.uid).toList();
      });
    } else if (result.updated != null) {
      setState(() {
        _messages = [
          for (final m in _messages)
            if (m.uid == msg.uid) result.updated! else m,
        ];
      });
    }
  }

  Future<void> _openCompose({
    String initialTo = '',
    String initialSubject = '',
    String initialBody = '',
  }) async {
    final sent = await showModalBottomSheet<bool>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (_) => _ComposeMailSheet(
        client: widget.app.client,
        account: _selectedAccount,
        initialTo: initialTo,
        initialSubject: initialSubject,
        initialBody: initialBody,
      ),
    );
    if (sent == true && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Message sent')),
      );
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
        _password.clear();
      });
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
      final acct = (_selectedAccount ?? _account.text).trim();
      await widget.app.client.mailDisconnect(
        account: acct.isEmpty ? null : acct,
      );
      if (!mounted) return;
      setState(() {
        _selectedAccount = null;
        _lastConnect = null;
        _password.clear();
        _busy = false;
        _showSettings = false;
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
    final connected = _isConnected;
    final showInbox = connected && !_showSettings;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Mail (SMTP / IMAP)'),
        actions: [
          if (connected)
            IconButton(
              key: const Key('mail-compose'),
              tooltip: 'Compose',
              onPressed: () => _openCompose(),
              icon: const Icon(Icons.edit_outlined),
            ),
          if (connected)
            IconButton(
              key: const Key('mail-settings-toggle'),
              tooltip: _showSettings ? 'Inbox' : 'Connection settings',
              onPressed: () => setState(() => _showSettings = !_showSettings),
              icon: Icon(
                _showSettings ? Icons.inbox_outlined : Icons.settings_outlined,
              ),
            ),
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: _loading && _status == null && _error == null
          ? const Center(child: CircularProgressIndicator())
          : showInbox
          ? _buildInboxView(context)
          : _buildSettingsView(context),
    );
  }

  Widget _buildInboxView(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final accounts = _status?.accounts ?? const <MailAccountStatus>[];
    final validValues = accounts
        .map((a) => a.account.isEmpty ? null : a.account)
        .toList();
    final dropdownValue = validValues.contains(_selectedAccount)
        ? _selectedAccount
        : (validValues.isNotEmpty ? validValues.first : null);
    return Column(
      children: [
        if (accounts.length > 1)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
            child: DropdownButtonFormField<String?>(
              key: ValueKey('mail-account-${dropdownValue ?? ''}'),
              initialValue: dropdownValue,
              decoration: const InputDecoration(
                labelText: 'Mailbox',
                border: OutlineInputBorder(),
                isDense: true,
              ),
              items: [
                for (final a in accounts)
                  DropdownMenuItem<String?>(
                    value: a.account.isEmpty ? null : a.account,
                    child: Text(
                      a.account.isEmpty
                          ? 'Default (${a.username ?? ''})'
                          : '${a.account} (${a.username ?? ''})',
                    ),
                  ),
              ],
              onChanged: (v) {
                setState(() {
                  _selectedAccount = v;
                  _selectedFolder = 'INBOX';
                  _activeSearch = '';
                  _searchController.clear();
                });
                _loadMailbox();
              },
            ),
          ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
          child: TextField(
            key: const Key('mail-search-input'),
            controller: _searchController,
            textInputAction: TextInputAction.search,
            onSubmitted: (_) => _runSearch(),
            onChanged: (_) => setState(() {}),
            decoration: InputDecoration(
              hintText: 'Search $_selectedFolder...',
              prefixIcon: const Icon(Icons.search),
              border: const OutlineInputBorder(),
              isDense: true,
              suffixIcon: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (_searchController.text.isNotEmpty ||
                      _activeSearch.isNotEmpty)
                    IconButton(
                      key: const Key('mail-search-clear'),
                      tooltip: 'Clear search',
                      icon: const Icon(Icons.close),
                      onPressed: _clearSearch,
                    ),
                  IconButton(
                    key: const Key('mail-search-submit'),
                    tooltip: 'Search',
                    icon: const Icon(Icons.arrow_forward),
                    onPressed: _runSearch,
                  ),
                ],
              ),
            ),
          ),
        ),
        SizedBox(
          height: 44,
          child: ListView.separated(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16),
            itemCount: _folders.length,
            separatorBuilder: (_, _) => const SizedBox(width: 8),
            itemBuilder: (context, index) {
              final folder = _folders[index];
              final selected = folder == _selectedFolder;
              return ChoiceChip(
                key: Key('mail-folder-$folder'),
                label: Text(folder),
                selected: selected,
                onSelected: (isSelected) {
                  if (!isSelected || selected) return;
                  setState(() {
                    _selectedFolder = folder;
                    _activeSearch = '';
                    _searchController.clear();
                  });
                  _loadMailbox();
                },
              );
            },
          ),
        ),
        const SizedBox(height: 4),
        if (_messagesError != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
            child: ErrorBanner(_messagesError!, onRetry: _loadMailbox),
          ),
        Expanded(
          child: _loadingMessages
              ? const Center(child: CircularProgressIndicator())
              : _messages.isEmpty
              ? Center(
                  child: Text(
                    _activeSearch.isNotEmpty
                        ? 'No messages matching "$_activeSearch".'
                        : 'No messages in $_selectedFolder.',
                  ),
                )
              : RefreshIndicator(
                  onRefresh: _loadMailbox,
                  child: ListView.builder(
                    padding: const EdgeInsets.fromLTRB(12, 4, 12, 24),
                    itemCount: _messages.length,
                    itemBuilder: (context, index) {
                      final msg = _messages[index];
                      final unread = !msg.isSeen;
                      return Card(
                        key: Key('mail-msg-${msg.uid}'),
                        child: ListTile(
                          onTap: () => _openMessage(msg),
                          leading: Icon(
                            unread
                                ? Icons.mark_email_unread
                                : Icons.drafts_outlined,
                            color: unread ? scheme.primary : null,
                          ),
                          title: Text(
                            msg.subject.isEmpty ? '(No subject)' : msg.subject,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(
                              fontWeight: unread
                                  ? FontWeight.w700
                                  : FontWeight.normal,
                            ),
                          ),
                          isThreeLine: msg.snippet.isNotEmpty,
                          subtitle: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                '${msg.from}${msg.date.isNotEmpty ? ' • ${msg.date}' : ''}',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              if (msg.snippet.isNotEmpty)
                                Text(
                                  msg.snippet,
                                  key: Key('mail-snippet-${msg.uid}'),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: Theme.of(context).textTheme.bodySmall,
                                ),
                            ],
                          ),
                          trailing: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              IconButton(
                                key: Key('mail-star-${msg.uid}'),
                                tooltip: msg.isFlagged ? 'Unstar' : 'Star',
                                icon: Icon(
                                  msg.isFlagged
                                      ? Icons.star
                                      : Icons.star_border,
                                  color: msg.isFlagged ? scheme.primary : null,
                                ),
                                onPressed: () => _toggleFlag(
                                  msg,
                                  flagged: !msg.isFlagged,
                                ),
                              ),
                              IconButton(
                                key: Key('mail-read-${msg.uid}'),
                                tooltip: unread
                                    ? 'Mark as read'
                                    : 'Mark as unread',
                                icon: Icon(
                                  unread
                                      ? Icons.mark_email_read_outlined
                                      : Icons.mark_email_unread_outlined,
                                ),
                                onPressed: () => _toggleFlag(
                                  msg,
                                  seen: unread,
                                ),
                              ),
                              IconButton(
                                key: Key('mail-delete-${msg.uid}'),
                                tooltip: 'Delete',
                                icon: const Icon(Icons.delete_outline),
                                onPressed: () => _deleteMessage(msg),
                              ),
                            ],
                          ),
                        ),
                      );
                    },
                  ),
                ),
        ),
      ],
    );
  }

  Widget _buildSettingsView(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final connected = _isConnected;
    return ListView(
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
                      connected ? Icons.check_circle : Icons.link_off,
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

class _DetailOutcome {
  const _DetailOutcome({this.deleted = false, this.updated});
  final bool deleted;
  final MailMessageInfo? updated;
}

class _MailMessageDetailScreen extends StatefulWidget {
  const _MailMessageDetailScreen({
    required this.app,
    required this.account,
    required this.folder,
    required this.initial,
  });

  final AppState app;
  final String? account;
  final String folder;
  final MailMessageInfo initial;

  @override
  State<_MailMessageDetailScreen> createState() =>
      _MailMessageDetailScreenState();
}

class _MailMessageDetailScreenState extends State<_MailMessageDetailScreen> {
  late MailMessageInfo _msg = widget.initial;
  bool _loading = true;
  String? _error;
  final Set<int> _savingAttachments = {};

  @override
  void initState() {
    super.initState();
    _fetchFull();
  }

  Future<void> _fetchFull() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final full = await widget.app.client.mailMessage(
        account: widget.account,
        folder: widget.folder,
        uid: widget.initial.uid,
      );
      if (!mounted) return;
      setState(() {
        _msg = full;
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  Future<void> _toggleFlag({bool? seen, bool? flagged}) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await widget.app.client.mailFlag(
        account: widget.account,
        folder: widget.folder,
        uid: _msg.uid,
        seen: seen,
        flagged: flagged,
      );
      if (!mounted) return;
      setState(() {
        _msg = _msg.withUpdatedFlags(seen: seen, flagged: flagged);
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _delete() async {
    final ok = await confirmAction(
      context,
      title: 'Delete message?',
      message:
          'Permanently delete "${_msg.subject.isEmpty ? '(No subject)' : _msg.subject}"?',
      confirmLabel: 'Delete',
    );
    if (!ok || !mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await widget.app.client.mailDelete(
        account: widget.account,
        folder: widget.folder,
        uid: _msg.uid,
      );
      if (!mounted) return;
      messenger.showSnackBar(
        const SnackBar(content: Text('Message deleted')),
      );
      Navigator.of(context).pop(const _DetailOutcome(deleted: true));
    } on ApiException catch (e) {
      if (!mounted) return;
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _saveAttachment(int index, MailAttachmentInfo att) async {
    setState(() => _savingAttachments.add(index));
    final messenger = ScaffoldMessenger.of(context);
    try {
      final saved = await widget.app.client.mailSaveAttachmentToLibrary(
        account: widget.account,
        folder: widget.folder,
        uid: _msg.uid,
        index: index,
      );
      if (!mounted) return;
      final label = saved.filename?.isNotEmpty == true
          ? saved.filename!
          : att.name;
      messenger
        ..clearSnackBars()
        ..showSnackBar(
          SnackBar(content: Text('Saved "$label" to Library')),
        );
    } on ApiException catch (e) {
      if (!mounted) return;
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    } finally {
      if (mounted) {
        setState(() => _savingAttachments.remove(index));
      }
    }
  }

  String _extractEmailAddr(String raw) {
    final trimmed = raw.trim();
    final start = trimmed.lastIndexOf('<');
    final end = trimmed.lastIndexOf('>');
    if (start >= 0 && end > start) {
      return trimmed.substring(start + 1, end).trim();
    }
    return trimmed;
  }

  Future<void> _reply() async {
    final subj = _msg.subject.trim();
    final replySubject = subj.toLowerCase().startsWith('re:')
        ? subj
        : 'Re: $subj';
    final quoted = _msg.body.isEmpty
        ? ''
        : '\n\n> ${_msg.body.replaceAll('\n', '\n> ')}';
    final sent = await showModalBottomSheet<bool>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (_) => _ComposeMailSheet(
        client: widget.app.client,
        account: widget.account,
        initialTo: _extractEmailAddr(_msg.from),
        initialSubject: replySubject,
        initialBody: quoted,
      ),
    );
    if (sent == true && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Reply sent')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final unread = !_msg.isSeen;
    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) {
          Navigator.of(context).pop(_DetailOutcome(updated: _msg));
        }
      },
      child: Scaffold(
        appBar: AppBar(
          title: const Text('Message'),
          leading: BackButton(
            onPressed: () =>
                Navigator.of(context).pop(_DetailOutcome(updated: _msg)),
          ),
          actions: [
            IconButton(
              key: const Key('mail-detail-star'),
              tooltip: _msg.isFlagged ? 'Unstar' : 'Star',
              icon: Icon(
                _msg.isFlagged ? Icons.star : Icons.star_border,
                color: _msg.isFlagged ? scheme.primary : null,
              ),
              onPressed: () => _toggleFlag(flagged: !_msg.isFlagged),
            ),
            IconButton(
              key: const Key('mail-detail-read'),
              tooltip: unread ? 'Mark as read' : 'Mark as unread',
              icon: Icon(
                unread
                    ? Icons.mark_email_read_outlined
                    : Icons.mark_email_unread_outlined,
              ),
              onPressed: () => _toggleFlag(seen: unread),
            ),
            IconButton(
              key: const Key('mail-detail-reply'),
              tooltip: 'Reply',
              icon: const Icon(Icons.reply),
              onPressed: _reply,
            ),
            IconButton(
              key: const Key('mail-detail-delete'),
              tooltip: 'Delete',
              icon: const Icon(Icons.delete_outline),
              onPressed: _delete,
            ),
          ],
        ),
        body: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            if (_error != null) ...[
              ErrorBanner(_error!, onRetry: _fetchFull),
              const SizedBox(height: 12),
            ],
            Text(
              _msg.subject.isEmpty ? '(No subject)' : _msg.subject,
              style: Theme.of(context).textTheme.headlineSmall,
            ),
            const SizedBox(height: 8),
            Text('From: ${_msg.from}'),
            if (_msg.date.isNotEmpty) Text('Date: ${_msg.date}'),
            const Divider(height: 24),
            if (_loading && _msg.body.isEmpty)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 24),
                child: Center(child: CircularProgressIndicator()),
              )
            else
              SelectableText(
                _msg.body.isEmpty ? '(Empty body)' : _msg.body,
              ),
            if (_msg.attachments.isNotEmpty) ...[
              const Divider(height: 32),
              Text(
                'Attachments (${_msg.attachments.length})',
                style: Theme.of(context).textTheme.titleSmall,
              ),
              const SizedBox(height: 8),
              for (var i = 0; i < _msg.attachments.length; i++)
                Card(
                  key: Key('mail-attachment-$i'),
                  child: ListTile(
                    leading: const Icon(Icons.attach_file),
                    title: Text(_msg.attachments[i].name),
                    subtitle: Text(
                      '${_msg.attachments[i].contentType} • ${_msg.attachments[i].size} B',
                    ),
                    trailing: FilledButton.tonalIcon(
                      key: Key('mail-save-attachment-$i'),
                      onPressed: _savingAttachments.contains(i)
                          ? null
                          : () => _saveAttachment(i, _msg.attachments[i]),
                      icon: const Icon(Icons.library_add_outlined, size: 18),
                      label: Text(
                        _savingAttachments.contains(i)
                            ? 'Saving...'
                            : 'Save to Library',
                      ),
                    ),
                  ),
                ),
            ],
          ],
        ),
      ),
    );
  }
}

class _ComposeMailSheet extends StatefulWidget {
  const _ComposeMailSheet({
    required this.client,
    required this.account,
    this.initialTo = '',
    this.initialSubject = '',
    this.initialBody = '',
  });

  final ApiClient client;
  final String? account;
  final String initialTo;
  final String initialSubject;
  final String initialBody;

  @override
  State<_ComposeMailSheet> createState() => _ComposeMailSheetState();
}

class _ComposeMailSheetState extends State<_ComposeMailSheet> {
  late final _to = TextEditingController(text: widget.initialTo);
  late final _subject = TextEditingController(text: widget.initialSubject);
  late final _body = TextEditingController(text: widget.initialBody);
  bool _sending = false;
  String? _error;

  @override
  void dispose() {
    _to.dispose();
    _subject.dispose();
    _body.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final recipients = _to.text
        .split(RegExp(r'[,;]'))
        .map((e) => e.trim())
        .where((e) => e.isNotEmpty)
        .toList();
    setState(() {
      _sending = true;
      _error = null;
    });
    try {
      await widget.client.mailSend(
        account: widget.account,
        to: recipients,
        subject: _subject.text,
        body: _body.text,
      );
      if (!mounted) return;
      Navigator.of(context).pop(true);
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _sending = false;
        _error = e.messageForUser();
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final bottom = MediaQuery.of(context).viewInsets.bottom;
    return Padding(
      padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + bottom),
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    widget.initialTo.isNotEmpty ? 'Reply' : 'Compose mail',
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ),
                IconButton(
                  tooltip: 'Close',
                  onPressed: _sending ? null : () => Navigator.of(context).pop(),
                  icon: const Icon(Icons.close),
                ),
              ],
            ),
            if (_error != null) ...[
              ErrorBanner(_error!),
              const SizedBox(height: 8),
            ],
            const SizedBox(height: 8),
            TextField(
              key: const Key('mail-compose-to'),
              controller: _to,
              keyboardType: TextInputType.emailAddress,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: 'To',
                hintText: 'alice@example.com, bob@example.com',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            TextField(
              key: const Key('mail-compose-subject'),
              controller: _subject,
              decoration: const InputDecoration(
                labelText: 'Subject',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            TextField(
              key: const Key('mail-compose-body'),
              controller: _body,
              maxLines: 6,
              decoration: const InputDecoration(
                labelText: 'Message',
                border: OutlineInputBorder(),
                alignLabelWithHint: true,
              ),
            ),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const Key('mail-compose-send'),
              onPressed: _sending ? null : _send,
              icon: const Icon(Icons.send),
              label: Text(_sending ? 'Sending...' : 'Send'),
            ),
          ],
        ),
      ),
    );
  }
}
