import 'package:flutter/material.dart';

import '../api_client.dart';
import '../models.dart';
import '../session.dart';
import '../validation.dart';

/// Owner fleet edits: disabled specialists and conversation nodes.
class FleetTab extends StatefulWidget {
  const FleetTab({super.key, required this.session, required this.onUseInChat});

  final OwnerSession session;
  final ValueChanged<String> onUseInChat;

  @override
  State<FleetTab> createState() => _FleetTabState();
}

class _FleetTabState extends State<FleetTab> {
  final _botId = TextEditingController();
  final _description = TextEditingController();
  final _skills = TextEditingController();
  final _slug = TextEditingController();
  final _parent = TextEditingController();
  final List<bool> _toolOn = specialistTools
      .map(defaultSpecialistTools.contains)
      .toList();

  ConversationKind _kind = ConversationKind.section;
  CreatedBot? _createdBot;
  CreatedConversation? _createdConversation;
  String? _error;
  bool _unknown = false;
  bool _submitting = false;
  DateTime _lastRequest = DateTime.fromMillisecondsSinceEpoch(0);

  @override
  void initState() {
    super.initState();
    widget.session.addListener(_onSession);
    _botId.addListener(_rebuild);
    _description.addListener(_rebuild);
    _skills.addListener(_rebuild);
    _slug.addListener(_rebuild);
    _parent.addListener(_rebuild);
  }

  @override
  void dispose() {
    widget.session.removeListener(_onSession);
    _botId.removeListener(_rebuild);
    _description.removeListener(_rebuild);
    _skills.removeListener(_rebuild);
    _slug.removeListener(_rebuild);
    _parent.removeListener(_rebuild);
    _botId.dispose();
    _description.dispose();
    _skills.dispose();
    _slug.dispose();
    _parent.dispose();
    super.dispose();
  }

  void _onSession() {
    if (mounted) {
      setState(() {});
    }
  }

  void _rebuild() {
    if (mounted) {
      setState(() {});
    }
  }

  List<String> _selectedTools() {
    final tools = <String>[];
    for (var i = 0; i < specialistTools.length; i++) {
      if (_toolOn[i]) {
        tools.add(specialistTools[i]);
      }
    }
    return tools;
  }

  Future<void> _throttle() async {
    final elapsed = DateTime.now().difference(_lastRequest);
    if (elapsed < const Duration(seconds: 3)) {
      await Future<void>.delayed(const Duration(seconds: 3) - elapsed);
    }
    _lastRequest = DateTime.now();
  }

  bool get _blocked =>
      _submitting || _unknown || !widget.session.connected || widget.session.busy;

  String? _skillError() {
    try {
      parseSkillList(_skills.text);
      return null;
    } on ValidationException catch (e) {
      return e.message;
    }
  }

  String? _toolError() {
    try {
      validateToolList(_selectedTools());
      return null;
    } on ValidationException catch (e) {
      return e.message;
    }
  }

  String? get _parentValue {
    if (_kind == ConversationKind.section) {
      return null;
    }
    final trimmed = _parent.text.trim();
    return trimmed.isEmpty ? null : trimmed;
  }

  String? _parentError() {
    try {
      validateConversationParent(_kind, _parentValue);
      return null;
    } on ValidationException catch (e) {
      return e.message;
    }
  }

  Future<void> _createBot() async {
    if (_blocked) {
      return;
    }
    setState(() {
      _submitting = true;
      _error = null;
    });
    try {
      await _throttle();
      if (!mounted || !widget.session.connected) {
        return;
      }
      final skills = parseSkillList(_skills.text);
      final bot = await widget.session.run(
        (client) => client.createBot(
          _botId.text.trim(),
          _description.text,
          _selectedTools(),
          skills,
        ),
      );
      if (!mounted) {
        return;
      }
      setState(() => _createdBot = bot);
    } on ValidationException catch (e) {
      if (mounted) {
        setState(() => _error = e.message);
      }
    } on ApiException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.messageForUser();
          if (e.unknownOutcome) {
            _unknown = true;
          }
        });
      }
    } finally {
      if (mounted) {
        setState(() => _submitting = false);
      }
    }
  }

  Future<void> _createConversation() async {
    if (_blocked) {
      return;
    }
    setState(() {
      _submitting = true;
      _error = null;
    });
    try {
      await _throttle();
      if (!mounted || !widget.session.connected) {
        return;
      }
      final node = await widget.session.run(
        (client) => client.createConversation(_kind, _slug.text.trim(), _parentValue),
      );
      if (!mounted) {
        return;
      }
      setState(() => _createdConversation = node);
    } on ApiException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.messageForUser();
          if (e.unknownOutcome) {
            _unknown = true;
          }
        });
      }
    } finally {
      if (mounted) {
        setState(() => _submitting = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final tools = _selectedTools();
    final skillError = _skillError();
    final toolError = _toolError();
    final botId = _botId.text.trim();
    final canCreateBot = !_blocked &&
        validBotId(botId) &&
        validBotDescription(_description.text) &&
        toolError == null &&
        skillError == null;
    final slug = _slug.text.trim();
    final parentError = _parentError();
    final canCreateConversation =
        !_blocked && validSlug(slug) && parentError == null;
    final preview = conversationNodeId(_kind, slug);
    final muted = Theme.of(context).textTheme.bodySmall;

    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text('Fleet', style: Theme.of(context).textTheme.headlineSmall),
        const SizedBox(height: 4),
        Text(
          'Disabled specialists and conversation nodes. Nothing is written until you submit.',
          style: muted,
        ),
        if (_error != null) ...[
          const SizedBox(height: 12),
          Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
        ],
        if (_unknown) ...[
          const SizedBox(height: 8),
          const Text(
            'A missing reply does not prove failure. Review server state before trying again.',
          ),
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: () => setState(() {
                _unknown = false;
                _error = null;
              }),
              child: const Text('I reviewed server state'),
            ),
          ),
        ],
        const SizedBox(height: 16),
        _card(
          context,
          title: 'New specialist',
          children: [
            Text(
              'The id is the bot name. The description is stored in the prompt stub. The bot stays disabled until you restart, and it is not added to main.delegates.',
              style: muted,
            ),
            const SizedBox(height: 12),
            TextField(
              controller: _botId,
              maxLength: 32,
              decoration: const InputDecoration(
                labelText: 'Bot id',
                hintText: 'notes',
                border: OutlineInputBorder(),
              ),
              autocorrect: false,
              enableSuggestions: false,
            ),
            if (botId.isNotEmpty && !validBotId(botId))
              Text(
                'Use a lowercase slug (max 32). main and Windows device names are reserved.',
                style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 12),
              ),
            const SizedBox(height: 8),
            TextField(
              controller: _description,
              minLines: 3,
              maxLines: 6,
              maxLength: 2000,
              decoration: const InputDecoration(
                labelText: 'Description',
                hintText: 'Synthetic notes specialist.',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            Text('Tools', style: Theme.of(context).textTheme.titleSmall),
            Text(
              'Specialist allowlist only. delegate is not offered.',
              style: muted,
            ),
            const SizedBox(height: 4),
            SizedBox(
              height: 200,
              child: ListView.builder(
                itemCount: specialistTools.length,
                itemBuilder: (context, index) {
                  return CheckboxListTile(
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    title: Text(specialistTools[index]),
                    value: _toolOn[index],
                    onChanged: (on) => setState(() => _toolOn[index] = on ?? false),
                  );
                },
              ),
            ),
            if (toolError != null)
              Text(
                toolError,
                style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 12),
              )
            else if (tools.isEmpty)
              Text(
                'No tools selected. The bot file will list an empty tool set.',
                style: muted,
              ),
            const SizedBox(height: 8),
            TextField(
              controller: _skills,
              decoration: const InputDecoration(
                labelText: 'Skills',
                hintText: 'ask, notes',
                border: OutlineInputBorder(),
                helperText:
                    'Optional skill ids, separated by commas. The server rejects unknown skills.',
              ),
              autocorrect: false,
              enableSuggestions: false,
            ),
            if (_skills.text.trim().isNotEmpty && skillError != null)
              Text(
                skillError,
                style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 12),
              ),
            if (_createdBot != null) ...[
              const SizedBox(height: 8),
              Text('Created ${_createdBot!.id} · disabled · not live'),
              Text(
                '${_createdBot!.botFile} · ${_createdBot!.promptFile}',
                style: muted,
              ),
              if (_createdBot!.requireApproval.isNotEmpty)
                Text(
                  'Approval required · ${_createdBot!.requireApproval.join(', ')}',
                  style: muted,
                ),
              Text(
                'Restart the server before this bot can run.',
                style: TextStyle(color: Theme.of(context).colorScheme.tertiary, fontSize: 12),
              ),
            ],
            const SizedBox(height: 12),
            FilledButton(
              onPressed: canCreateBot ? _createBot : null,
              child: const Text('Create disabled bot'),
            ),
          ],
        ),
        const SizedBox(height: 16),
        _card(
          context,
          title: 'New conversation',
          children: [
            Text(
              'The server assigns the id. Sections and groups do not accept turns. Direct and room nodes do.',
              style: muted,
            ),
            const SizedBox(height: 12),
            DropdownButtonFormField<ConversationKind>(
              initialValue: _kind,
              decoration: const InputDecoration(
                labelText: 'Kind',
                border: OutlineInputBorder(),
              ),
              items: ConversationKind.values
                  .map(
                    (kind) => DropdownMenuItem(value: kind, child: Text(kind.label)),
                  )
                  .toList(),
              onChanged: (kind) {
                if (kind == null) {
                  return;
                }
                setState(() {
                  _kind = kind;
                  if (kind == ConversationKind.section) {
                    _parent.clear();
                  }
                });
              },
            ),
            const SizedBox(height: 8),
            TextField(
              controller: _slug,
              maxLength: 32,
              decoration: const InputDecoration(
                labelText: 'Slug',
                hintText: 'finance',
                border: OutlineInputBorder(),
              ),
              autocorrect: false,
              enableSuggestions: false,
            ),
            if (preview != null)
              Text('Server id $preview', style: muted)
            else if (slug.isNotEmpty)
              Text(
                'Slug must be a lowercase slug of at most 32 characters.',
                style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 12),
              ),
            const SizedBox(height: 4),
            Text(_kind.parentHint, style: muted),
            if (_kind != ConversationKind.section) ...[
              const SizedBox(height: 8),
              TextField(
                controller: _parent,
                decoration: InputDecoration(
                  labelText: _kind == ConversationKind.group ? 'Parent section' : 'Parent',
                  hintText: _kind == ConversationKind.group
                      ? 'sec:finance'
                      : 'sec:finance or grp:ops',
                  border: const OutlineInputBorder(),
                ),
                autocorrect: false,
                enableSuggestions: false,
              ),
              if (_parent.text.trim().isNotEmpty && parentError != null)
                Text(
                  parentError,
                  style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 12),
                ),
            ],
            if (_createdConversation != null) ...[
              const SizedBox(height: 8),
              Text(
                'Created ${_createdConversation!.id} · ${_createdConversation!.acceptsTurns ? 'accepts turns' : 'does not accept turns'}',
              ),
              Text(
                'Sections and groups still reject turns.',
                style: muted,
              ),
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton(
                  onPressed: () {
                    final id = _createdConversation!.id;
                    if (validId(id)) {
                      widget.onUseInChat(id);
                    }
                  },
                  child: const Text('Use in chat'),
                ),
              ),
            ],
            const SizedBox(height: 12),
            FilledButton(
              onPressed: canCreateConversation ? _createConversation : null,
              child: const Text('Create conversation'),
            ),
          ],
        ),
      ],
    );
  }

  Widget _card(BuildContext context, {required String title, required List<Widget> children}) {
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(title, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            ...children,
          ],
        ),
      ),
    );
  }
}
