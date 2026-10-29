import 'dart:async';

import 'package:flutter/material.dart';

import '../api_client.dart';
import '../models.dart';
import '../session.dart';

class ChatTab extends StatefulWidget {
  const ChatTab({super.key, required this.session});

  final OwnerSession session;

  @override
  State<ChatTab> createState() => _ChatTabState();
}

class _ChatTabState extends State<ChatTab> {
  final _composer = TextEditingController();
  final _scroll = ScrollController();
  List<ChatMessage> _messages = [];
  Catalog? _catalog;
  Selection? _pendingSelection;
  String? _banner;
  String? _trackedTaskId;
  TaskStatus? _trackedStatus;
  Timer? _pollTimer;
  DateTime _lastRequest = DateTime.fromMillisecondsSinceEpoch(0);
  int _pollTick = 0;
  late String _loadedConversation;

  @override
  void initState() {
    super.initState();
    _loadedConversation = widget.session.conversationId;
    widget.session.addListener(_onSessionChanged);
    _pollTimer = Timer.periodic(const Duration(seconds: 3), (_) => _poll());
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _refreshAll();
    });
  }

  @override
  void dispose() {
    widget.session.removeListener(_onSessionChanged);
    _pollTimer?.cancel();
    _composer.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _onSessionChanged() {
    if (!mounted) {
      return;
    }
    final id = widget.session.conversationId;
    if (id == _loadedConversation) {
      return;
    }
    _loadedConversation = id;
    setState(() {
      _messages = [];
      _banner = null;
      _trackedTaskId = null;
      _trackedStatus = null;
      _pendingSelection = null;
    });
    _refreshAll();
  }

  Future<void> _throttle() async {
    final elapsed = DateTime.now().difference(_lastRequest);
    if (elapsed < const Duration(seconds: 3)) {
      await Future<void>.delayed(const Duration(seconds: 3) - elapsed);
    }
    _lastRequest = DateTime.now();
  }

  Future<void> _refreshAll() async {
    await _loadModels();
    await _loadHistory();
  }

  Future<void> _poll() async {
    if (!widget.session.connected || widget.session.busy) {
      return;
    }
    _pollTick++;
    try {
      await _throttle();
      if (_pollTick % 2 == 0) {
        await _loadHistory(silent: true);
      }
      if (_trackedTaskId != null &&
          _trackedStatus != null &&
          !_trackedStatus!.terminal) {
        await _throttle();
        final t = await widget.session.client.task(_trackedTaskId!);
        if (!mounted) {
          return;
        }
        setState(() => _trackedStatus = t.status);
      }
    } on ApiException catch (e) {
      if (e.statusCode == 429) {
        setState(() => _banner = e.messageForUser());
      }
    } catch (_) {
      // Ignore transient poll errors.
    }
  }

  Future<void> _loadHistory({bool silent = false}) async {
    try {
      await _throttle();
      final items =
          await widget.session.client.history(widget.session.conversationId);
      if (!mounted) {
        return;
      }
      setState(() {
        _messages = items;
        if (!silent) {
          _banner = null;
        }
      });
      _scrollToEnd();
    } on ApiException catch (e) {
      if (!silent) {
        setState(() => _banner = e.messageForUser());
      }
    }
  }

  Future<void> _loadModels() async {
    try {
      await _throttle();
      final catalog =
          await widget.session.client.models(widget.session.conversationId);
      if (!mounted) {
        return;
      }
      setState(() {
        _catalog = catalog;
        _pendingSelection ??= catalog.selection;
      });
    } on ApiException catch (e) {
      setState(() => _banner = e.messageForUser());
    }
  }

  void _scrollToEnd() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scroll.hasClients) {
        _scroll.jumpTo(_scroll.position.maxScrollExtent);
      }
    });
  }

  Future<void> _send() async {
    final text = _composer.text;
    if (text.trim().isEmpty) {
      return;
    }
    setState(() => _banner = null);
    try {
      final task = await widget.session.run(
        (c) => c.submit(widget.session.conversationId, text),
      );
      _composer.clear();
      setState(() {
        _trackedTaskId = task.id;
        _trackedStatus = task.status;
      });
      await _loadHistory();
    } on ApiException catch (e) {
      setState(() {
        if (e.unknownOutcome) {
          _banner =
              '${e.messageForUser()} Outcome unknown — check server history before resending.';
        } else {
          _banner = e.messageForUser();
        }
      });
    }
  }

  Future<void> _applyModel() async {
    final catalog = _catalog;
    final pending = _pendingSelection;
    if (catalog == null || pending == null) {
      return;
    }
    if (!catalog.allows(pending)) {
      setState(() {
        _banner =
            'That profile is not approved for data egress or is not in the catalog.';
      });
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Confirm model change'),
        content: Text(
          pending.isAuto
              ? 'Use automatic routing for future turns? The provider may receive this conversation\'s history.'
              : 'Use ${pending.provider} / ${pending.model} for future turns? '
                  'The provider will receive this conversation\'s history.',
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Apply')),
        ],
      ),
    );
    if (confirmed != true || !mounted) {
      return;
    }
    try {
      final applied = await widget.session.run(
        (c) => c.selectModel(widget.session.conversationId, pending),
      );
      setState(() {
        _catalog = Catalog(
          providers: catalog.providers,
          entries: catalog.entries,
          selection: applied,
          effectiveSelection: catalog.effectiveSelection,
        );
        _pendingSelection = applied;
        _banner = 'Model selection updated.';
      });
    } on ApiException catch (e) {
      setState(() => _banner = e.messageForUser());
    }
  }

  Widget _modelPicker() {
    final catalog = _catalog;
    final pending = _pendingSelection;
    if (catalog == null || pending == null) {
      return const LinearProgressIndicator();
    }

    final providers = catalog.providers;
    String? providerId = pending.isAuto ? null : pending.provider;
    String? modelId = pending.isAuto ? null : pending.model;

    ProviderInfo? selectedProvider;
    for (final p in providers) {
      if (p.id == providerId) {
        selectedProvider = p;
        break;
      }
    }

    return Card(
      margin: const EdgeInsets.all(8),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('Model routing', style: Theme.of(context).textTheme.titleSmall),
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: const Text('Automatic routing'),
              value: pending.isAuto,
              onChanged: (on) {
                setState(() {
                  _pendingSelection =
                      on ? Selection.auto() : catalog.selection;
                });
              },
            ),
            if (!pending.isAuto) ...[
              DropdownButtonFormField<String>(
                initialValue: providerId != null && providers.any((p) => p.id == providerId)
                    ? providerId
                    : null,
                decoration: const InputDecoration(labelText: 'Provider'),
                items: providers
                    .map(
                      (p) => DropdownMenuItem(
                        value: p.id,
                        child: Text(
                          p.dataEgressApproved ? p.id : '${p.id} (egress not approved)',
                        ),
                      ),
                    )
                    .toList(),
                onChanged: (v) {
                  if (v == null) {
                    return;
                  }
                  final p = providers.firstWhere((e) => e.id == v);
                  setState(() {
                    _pendingSelection = Selection(
                      provider: v,
                      model: p.models.isNotEmpty ? p.models.first : '',
                    );
                  });
                },
              ),
              if (selectedProvider != null)
                DropdownButtonFormField<String>(
                  initialValue: modelId != null && selectedProvider.models.contains(modelId)
                      ? modelId
                      : null,
                  decoration: const InputDecoration(labelText: 'Model'),
                  items: selectedProvider.models
                      .map((m) => DropdownMenuItem(value: m, child: Text(m)))
                      .toList(),
                  onChanged: (v) {
                    if (v == null || providerId == null) {
                      return;
                    }
                    setState(() {
                      _pendingSelection = Selection(provider: providerId, model: v);
                    });
                  },
                ),
            ],
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton.tonal(
                onPressed: widget.session.busy ? null : _applyModel,
                child: const Text('Apply selection'),
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        if (_banner != null)
          MaterialBanner(
            content: Text(_banner!),
            actions: [
              TextButton(onPressed: () => setState(() => _banner = null), child: const Text('Dismiss')),
            ],
          ),
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 4, 12, 0),
          child: Align(
            alignment: Alignment.centerLeft,
            child: Text(
              widget.session.conversationId,
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
        ),
        if (_trackedTaskId != null && _trackedStatus != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
            child: Text(
              'Task ${_trackedTaskId!.substring(0, 8)}… — ${_trackedStatus!.label}',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
        _modelPicker(),
        Expanded(
          child: RefreshIndicator(
            onRefresh: _refreshAll,
            child: ListView.builder(
              controller: _scroll,
              padding: const EdgeInsets.all(12),
              itemCount: _messages.length,
              itemBuilder: (context, index) {
                final m = _messages[index];
                final isUser = m.role == MessageRole.user;
                return Align(
                  alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
                  child: Container(
                    margin: const EdgeInsets.symmetric(vertical: 4),
                    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                    constraints: BoxConstraints(
                      maxWidth: MediaQuery.sizeOf(context).width * 0.85,
                    ),
                    decoration: BoxDecoration(
                      color: isUser
                          ? Theme.of(context).colorScheme.primaryContainer
                          : Theme.of(context).colorScheme.surfaceContainerHighest,
                      borderRadius: BorderRadius.circular(12),
                    ),
                    child: Text(m.content),
                  ),
                );
              },
            ),
          ),
        ),
        SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Expanded(
                  child: TextField(
                    controller: _composer,
                    minLines: 1,
                    maxLines: 6,
                    decoration: const InputDecoration(
                      hintText: 'Message',
                      border: OutlineInputBorder(),
                    ),
                    onSubmitted: (_) => _send(),
                  ),
                ),
                const SizedBox(width: 8),
                IconButton.filled(
                  onPressed: widget.session.busy ? null : _send,
                  icon: const Icon(Icons.send),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
