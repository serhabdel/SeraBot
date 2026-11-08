import 'package:flutter/material.dart';

import '../abilities.dart';
import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../device.dart';
import '../models.dart';
import '../widgets.dart';

/// The user's bots (same `GET /v1/bots` as the desktop) and their settings.
class BotsScreen extends StatelessWidget {
  const BotsScreen({super.key, required this.app, required this.device});
  final AppState app;
  final DeviceActions device;

  Future<void> _create(BuildContext context) async {
    final name = await showDialog<String>(
      context: context,
      builder: (_) => const _NameDialog(),
    );
    if (name == null || !context.mounted) {
      return;
    }
    final messenger = ScaffoldMessenger.of(context);
    try {
      final id = await app.client.createNamedBot(name);
      await app.refreshBots();
      if (context.mounted) {
        _open(context, id);
      }
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  void _open(BuildContext context, String id) {
    Navigator.of(context).push(MaterialPageRoute<void>(
      builder: (_) => BotSettingsScreen(app: app, botId: id, device: device),
    ));
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) => Scaffold(
        appBar: AppBar(title: const Text('Bots')),
        floatingActionButton: FloatingActionButton.extended(
            heroTag: null,
          onPressed: () => _create(context),
          icon: const Icon(Icons.add),
          label: const Text('New bot'),
        ),
        body: RefreshIndicator(
          onRefresh: app.refreshBots,
          child: app.bots.isEmpty
              ? ListView(children: const [
                  SizedBox(height: 120),
                  Center(child: Text('No bots yet.')),
                ])
              : ListView(
                  padding: const EdgeInsets.only(bottom: 96),
                  children: [
                    for (final b in app.bots)
                      ListTile(
                        onTap: () => _open(context, b.id),
                        leading: SeraAvatar(
                          label: b.name,
                          emoji: b.avatar,
                          app: app,
                          id: b.id,
                          version: b.avatarVersion,
                        ),
                        title: Text(b.name),
                        subtitle: Text(
                          [
                            if (!b.enabled) 'Off',
                            if (b.model != null) b.model!,
                            if (b.description != null && b.description!.isNotEmpty)
                              b.description!,
                          ].join(' \u00b7 '),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                        trailing: const Icon(Icons.chevron_right),
                      ),
                  ],
                ),
        ),
      ),
    );
  }
}

class _NameDialog extends StatefulWidget {
  const _NameDialog();
  @override
  State<_NameDialog> createState() => _NameDialogState();
}

class _NameDialogState extends State<_NameDialog> {
  final _c = TextEditingController();
  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('New bot'),
      content: TextField(
        controller: _c,
        autofocus: true,
        maxLength: 60,
        decoration: const InputDecoration(labelText: 'Name'),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(
          onPressed: () => Navigator.pop(context, _c.text.trim()),
          child: const Text('Create'),
        ),
      ],
    );
  }
}

/// Name, picture, instructions, model and abilities of one bot
/// (`GET/PUT /v1/bots/{id}`, `PUT /v1/bots/{id}/avatar`).
class BotSettingsScreen extends StatefulWidget {
  const BotSettingsScreen({
    super.key,
    required this.app,
    required this.botId,
    required this.device,
  });
  final AppState app;
  final String botId;
  final DeviceActions device;

  @override
  State<BotSettingsScreen> createState() => _BotSettingsScreenState();
}

class _BotSettingsScreenState extends State<BotSettingsScreen> {
  final _name = TextEditingController();
  final _desc = TextEditingController();
  final _instr = TextEditingController();
  BotDetail? _detail;
  Catalog? _catalog;
  Selection? _model;
  bool _clearModel = false;
  List<String> _tools = [];
  bool _enabled = true;
  bool _loading = true;
  bool _saving = false;
  String? _error;
  int _avatarTick = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _name.dispose();
    _desc.dispose();
    _instr.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final d = await widget.app.client.botDetail(widget.botId);
      Catalog? cat;
      final firstChat = widget.app.conversations.where((c) => c.isChat);
      if (firstChat.isNotEmpty) {
        try {
          cat = await widget.app.client.models(firstChat.first.id);
        } on ApiException {
          cat = null;
        }
      }
      if (!mounted) {
        return;
      }
      setState(() {
        _detail = d;
        _catalog = cat;
        _name.text = d.displayName;
        _desc.text = d.description;
        _instr.text = d.instructions ?? '';
        _tools = List.of(d.tools);
        _enabled = d.enabled;
        _model = d.provider.isEmpty || d.model.isEmpty
            ? null
            : Selection(provider: d.provider, model: d.model);
        _loading = false;
      });
    } on ApiException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.messageForUser();
          _loading = false;
        });
      }
    }
  }

  Future<void> _save() async {
    final d = _detail!;
    setState(() {
      _saving = true;
      _error = null;
    });
    final changedModel = _model != null &&
        (_model!.provider != d.provider || _model!.model != d.model);
    final body = botSettingsBody(
      displayName: _name.text.trim() != d.displayName ? _name.text : null,
      description: _desc.text != d.description ? _desc.text : null,
      instructions: _instr.text != (d.instructions ?? '') ? _instr.text : null,
      enabled: _enabled != d.enabled ? _enabled : null,
      model: changedModel ? (provider: _model!.provider, model: _model!.model) : null,
      clearModel: _clearModel,
      tools: _sameTools(_tools, d.tools) ? null : _tools,
    );
    try {
      await widget.app.client.updateBot(widget.botId, body);
      await widget.app.refreshBots();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Saved')),
        );
        Navigator.of(context).pop();
      }
    } on ApiException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.messageForUser();
          _saving = false;
        });
      }
    }
  }

  bool _sameTools(List<String> a, List<String> b) =>
      a.length == b.length && a.toSet().containsAll(b);

  Future<void> _picture() async {
    try {
      final files = await widget.device.pickFiles(imagesOnly: true);
      if (files.isEmpty) {
        return;
      }
      await widget.app.client.setBotAvatar(widget.botId, files.first.bytes);
      await widget.app.refreshBots();
      if (mounted) {
        setState(() => _avatarTick++);
      }
    } on ApiException catch (e) {
      if (mounted) {
        setState(() => _error = e.messageForUser());
      }
    } catch (_) {
      // Picker cancelled.
    }
  }

  @override
  Widget build(BuildContext context) {
    final d = _detail;
    return Scaffold(
      appBar: AppBar(
        title: Text(d?.displayName ?? 'Bot'),
        actions: [
          if (d != null)
            TextButton(
              onPressed: _saving ? null : _save,
              child: const Text('Save'),
            ),
        ],
      ),
      body: _loading
          ? const Center(child: CircularProgressIndicator())
          : d == null
              ? Padding(
                  padding: const EdgeInsets.all(16),
                  child: ErrorBanner(_error ?? 'Could not load the bot.', onRetry: _load),
                )
              : ListView(
                  padding: const EdgeInsets.all(16),
                  children: [
                    if (_error != null) ...[
                      ErrorBanner(_error!),
                      const SizedBox(height: 12),
                    ],
                    Center(
                      child: Column(children: [
                        KeyedSubtree(
                          key: ValueKey(_avatarTick),
                          child: SeraAvatar(
                            label: d.displayName,
                            emoji: d.avatar,
                            app: widget.app,
                            id: d.id,
                            version: '${widget.app.botById(d.id)?.avatarVersion ?? ''}$_avatarTick',
                            radius: 36,
                          ),
                        ),
                        TextButton(
                          onPressed: _picture,
                          child: const Text('Change picture (PNG)'),
                        ),
                      ]),
                    ),
                    TextField(
                      controller: _name,
                      maxLength: 48,
                      decoration: const InputDecoration(labelText: 'Name'),
                    ),
                    const SizedBox(height: 8),
                    TextField(
                      controller: _desc,
                      maxLines: 3,
                      maxLength: 2000,
                      decoration: const InputDecoration(labelText: 'Description'),
                    ),
                    const SizedBox(height: 8),
                    TextField(
                      controller: _instr,
                      minLines: 4,
                      maxLines: 12,
                      decoration: const InputDecoration(
                        labelText: 'Instructions',
                        alignLabelWithHint: true,
                      ),
                    ),
                    SwitchListTile(
                      value: _enabled,
                      onChanged: (v) => setState(() => _enabled = v),
                      title: const Text('Enabled'),
                    ),
                    _modelPicker(context),
                    const SizedBox(height: 8),
                    Text('Abilities', style: Theme.of(context).textTheme.titleMedium),
                    for (final a in abilities)
                      SwitchListTile(
                        value: a.isOn(_tools),
                        onChanged: (v) => setState(() => _tools = a.toggled(_tools, v)),
                        title: Text(a.label),
                        subtitle: Text(a.needs == null ? a.hint : '${a.hint}\nNeeds ${a.needs}.'),
                      ),
                    Padding(
                      padding: const EdgeInsets.only(top: 8),
                      child: Text(
                        '${d.memoryCount} memories \u00b7 ${d.artifactCount} files',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ),
                  ],
                ),
    );
  }

  Widget _modelPicker(BuildContext context) {
    final cat = _catalog;
    final current = _clearModel
        ? 'Default'
        : (_model == null ? 'Default' : '${_model!.provider} \u00b7 ${_model!.model}');
    return ListTile(
      contentPadding: EdgeInsets.zero,
      title: const Text('Model'),
      subtitle: Text(current),
      trailing: const Icon(Icons.expand_more),
      onTap: cat == null
          ? null
          : () async {
              final picked = await showModalBottomSheet<Selection?>(
                context: context,
                showDragHandle: true,
                isScrollControlled: true,
                builder: (ctx) => SafeArea(
                  child: ListView(shrinkWrap: true, children: [
                    ListTile(
                      title: const Text('Default'),
                      onTap: () => Navigator.pop(ctx, Selection.auto()),
                    ),
                    for (final p in cat.providers)
                      ExpansionTile(
                        title: Text(p.id),
                        children: [
                          for (final m in p.models)
                            ListTile(
                              dense: true,
                              title: Text(m),
                              onTap: () => Navigator.pop(
                                ctx,
                                Selection(provider: p.id, model: m),
                              ),
                            ),
                        ],
                      ),
                  ]),
                ),
              );
              if (picked == null) {
                return;
              }
              setState(() {
                if (picked.isAuto) {
                  _clearModel = true;
                  _model = null;
                } else {
                  _clearModel = false;
                  _model = picked;
                }
              });
            },
    );
  }
}
