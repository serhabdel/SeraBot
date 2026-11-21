import 'dart:async';

import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../device.dart';
import '../models.dart';
import '../widgets.dart';
import 'chat_screen.dart';

/// Derives a clean, human-readable title for a conversation, avoiding raw
/// generated slugs like `dir:chat-...` or bare `Chat 01a1dfd96...`.
String smartConversationTitle(ConversationSummary c) {
  final raw = c.title?.trim() ?? '';
  final looksRaw = raw.isEmpty ||
      raw.startsWith('dir:') ||
      raw.startsWith('room:') ||
      RegExp(r'^chat[-_\s]+[0-9a-f]{6,}$', caseSensitive: false).hasMatch(raw) ||
      RegExp(r'^[0-9a-f]{8,}$', caseSensitive: false).hasMatch(raw);
  if (!looksRaw) {
    return raw;
  }
  final preview = c.lastMessage?.text.trim() ?? '';
  if (preview.isNotEmpty) {
    final firstLine = preview.split('\n').first.trim();
    if (firstLine.isNotEmpty) {
      return firstLine.length > 36
          ? '${firstLine.substring(0, 35)}\u2026'
          : firstLine;
    }
  }
  return c.isRoom ? 'Channel' : 'New conversation';
}

enum ChatFilter { all, bots, channels, unread }

/// Redesigned Chats list: search, quick `+ New Chat` / `+ New Channel`,
/// horizontal Bot Quick-Launch Strip, filter chips, and rich chat cards.
class ChatsScreen extends StatefulWidget {
  const ChatsScreen({super.key, required this.app, required this.device});

  final AppState app;
  final DeviceActions device;

  @override
  State<ChatsScreen> createState() => _ChatsScreenState();
}

class _ChatsScreenState extends State<ChatsScreen> {
  final _search = TextEditingController();
  bool _searching = false;
  ChatFilter _filter = ChatFilter.all;

  /// Full-text message matches from `GET /v1/conversations/search` (0.74.2),
  /// debounced while the member types. Title filtering stays instant and
  /// local; this section is the server search underneath it.
  Timer? _searchTimer;
  List<ConversationSearchHit> _hits = const [];
  bool _searchingServer = false;
  String? _searchError;
  int _searchGen = 0;

  AppState get app => widget.app;

  @override
  void dispose() {
    _searchTimer?.cancel();
    _search.dispose();
    super.dispose();
  }

  void _onSearchChanged(String _) {
    setState(() {});
    _searchTimer?.cancel();
    final q = _search.text.trim();
    if (q.runes.length < 2) {
      setState(() {
        _hits = const [];
        _searchError = null;
        _searchingServer = false;
      });
      return;
    }
    setState(() => _searchingServer = true);
    _searchTimer = Timer(const Duration(milliseconds: 350), () {
      _runServerSearch(q);
    });
  }

  Future<void> _runServerSearch(String q) async {
    final gen = ++_searchGen;
    try {
      final hits = await app.client.searchConversations(q, limit: 15);
      if (!mounted || gen != _searchGen) return;
      setState(() {
        _hits = hits;
        _searchError = null;
        _searchingServer = false;
      });
    } on ApiException catch (e) {
      if (!mounted || gen != _searchGen) return;
      setState(() {
        _hits = const [];
        _searchError = e.messageForUser();
        _searchingServer = false;
      });
    }
  }

  /// Opens the chat a message match belongs to, refreshing the list first
  /// when the conversation is not on screen (archived or beyond the page).
  Future<void> _openHit(ConversationSearchHit hit) async {
    ConversationSummary? found;
    for (final c in app.conversations) {
      if (c.id == hit.conversationId) {
        found = c;
        break;
      }
    }
    found ??= await (() async {
      await app.refreshConversations();
      for (final c in app.conversations) {
        if (c.id == hit.conversationId) return c;
      }
      return null;
    })();
    if (!mounted) return;
    if (found == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('That chat is no longer in your list.'),
        ),
      );
      return;
    }
    _open(found);
  }

  Future<void> _actions(ConversationSummary c) async {
    final choice = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              key: const Key('chat-rename'),
              leading: const Icon(Icons.edit_outlined),
              title: const Text('Rename'),
              onTap: () => Navigator.of(sheet).pop('rename'),
            ),
            ListTile(
              key: const Key('chat-avatar'),
              leading: const Icon(Icons.image_outlined),
              title: const Text('Channel image'),
              onTap: () => Navigator.of(sheet).pop('avatar'),
            ),
            ListTile(
              key: const Key('chat-archive'),
              leading: const Icon(Icons.archive_outlined),
              title: const Text('Archive'),
              onTap: () => Navigator.of(sheet).pop('archive'),
            ),
          ],
        ),
      ),
    );
    if (!mounted) return;
    if (choice == 'rename') {
      await _rename(c);
    } else if (choice == 'avatar') {
      await _pickChannelImage(c);
    } else if (choice == 'archive') {
      await _archive(c);
    }
  }

  Future<void> _pickChannelImage(ConversationSummary c) async {
    final files = await widget.device.pickFiles(imagesOnly: true);
    if (files.isEmpty || !mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await app.client.setConversationAvatar(c.id, files.first.bytes);
      await app.refreshConversations();
      if (mounted) {
        messenger.showSnackBar(
          const SnackBar(content: Text('Channel image updated')),
        );
      }
    } on ApiException catch (e) {
      if (mounted) {
        messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    }
  }

  Future<void> _rename(ConversationSummary c) async {
    final current = smartConversationTitle(c);
    final controller = TextEditingController(text: current);
    final title = await showDialog<String>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Rename chat'),
        content: TextField(
          key: const Key('rename-field'),
          controller: controller,
          autofocus: true,
          maxLength: 120,
          decoration: const InputDecoration(labelText: 'Name'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialog).pop(),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const Key('rename-save'),
            onPressed: () => Navigator.of(dialog).pop(controller.text),
            child: const Text('Save'),
          ),
        ],
      ),
    );
    controller.dispose();
    final next = title?.trim() ?? '';
    if (next.isEmpty || next == current || !mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await app.client.renameConversation(c.id, next);
      await app.refreshConversations();
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _archive(ConversationSummary c) async {
    final label = smartConversationTitle(c);
    final ok = await confirmAction(
      context,
      title: 'Archive this chat?',
      message: '"$label" leaves your list. You can undo right after.',
      confirmLabel: 'Archive',
    );
    if (!ok || !mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await app.client.archiveConversation(c.id);
      await app.refreshConversations();
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
      return;
    }
    messenger.showSnackBar(SnackBar(
      content: const Text('Chat archived'),
      action: SnackBarAction(
        label: 'Undo',
        onPressed: () async {
          try {
            await app.client.unarchiveConversation(c.id);
            await app.refreshConversations();
          } on ApiException catch (e) {
            messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
          }
        },
      ),
    ));
  }

  void _open(ConversationSummary c) {
    Navigator.of(context).push(MaterialPageRoute<void>(
      builder: (_) =>
          ChatScreen(app: app, conversation: c, device: widget.device),
    ));
  }

  Future<void> _newChat({bool channel = false}) async {
    final created = await showModalBottomSheet<ConversationSummary>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => NewChatSheet(app: app, channelMode: channel),
    );
    if (created != null && mounted) {
      await app.refreshConversations();
      if (mounted) {
        _open(created);
      }
    }
  }

  Future<void> _launchBot(BotSummary bot) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      final created = await app.client.createGroupChat(bot.name, [bot.id]);
      await app.refreshConversations();
      if (mounted) {
        _open(created);
      }
    } on ApiException catch (e) {
      if (mounted) {
        messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    }
  }

  List<ConversationBucket> _filteredBuckets(List<ConversationBucket> raw) {
    if (_filter == ChatFilter.all) return raw;
    bool keep(ConversationSummary c) {
      switch (_filter) {
        case ChatFilter.all:
          return true;
        case ChatFilter.bots:
          return !c.isRoom;
        case ChatFilter.channels:
          return c.isRoom;
        case ChatFilter.unread:
          return c.unreadCount > 0;
      }
    }

    final out = <ConversationBucket>[];
    for (final b in raw) {
      final kept = b.chats.where(keep).toList();
      if (kept.isNotEmpty) {
        final nb = ConversationBucket(b.folder)..chats.addAll(kept);
        out.add(nb);
      }
    }
    return out;
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        final buckets = _filteredBuckets(
          bucketConversations(app.conversations, query: _search.text),
        );
        return Scaffold(
          appBar: AppBar(
            title: _searching
                ? TextField(
                    controller: _search,
                    autofocus: true,
                    decoration: const InputDecoration(
                      hintText: 'Search chats',
                      border: InputBorder.none,
                    ),
                    onChanged: _onSearchChanged,
                  )
                : const Text('Chats'),
            actions: [
              IconButton(
                tooltip: _searching ? 'Close search' : 'Search',
                icon: Icon(_searching ? Icons.close : Icons.search),
                onPressed: () => setState(() {
                  _searching = !_searching;
                  if (!_searching) {
                    _searchTimer?.cancel();
                    _search.clear();
                    _hits = const [];
                    _searchError = null;
                    _searchingServer = false;
                  }
                }),
              ),
              IconButton(
                tooltip: 'New channel',
                icon: const Icon(Icons.tag_rounded),
                onPressed: () => _newChat(channel: true),
              ),
            ],
          ),
          floatingActionButton: FloatingActionButton.extended(
            heroTag: null,
            onPressed: () => _newChat(),
            icon: const Icon(Icons.edit_outlined),
            label: const Text('New chat'),
          ),
          body: RefreshIndicator(
            onRefresh: app.refreshConversations,
            child: _body(context, buckets),
          ),
        );
      },
    );
  }

  Widget _body(BuildContext context, List<ConversationBucket> buckets) {
    final enabledBots = app.bots.where((b) => b.enabled).toList();
    if (app.conversationsError != null && app.conversations.isEmpty) {
      return ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.all(16),
        children: [
          ErrorBanner(app.conversationsError!,
              onRetry: app.refreshConversations),
        ],
      );
    }
    if (app.loadingConversations && app.conversations.isEmpty) {
      return ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        children: const [
          SizedBox(height: 120),
          Center(child: CircularProgressIndicator()),
        ],
      );
    }

    return ListView(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.only(bottom: 96),
      children: [
        if (app.conversationsError != null)
          Padding(
            padding: const EdgeInsets.all(12),
            child: ErrorBanner(app.conversationsError!,
                onRetry: app.refreshConversations),
          ),
        if (enabledBots.isNotEmpty) _botQuickStrip(context, enabledBots),
        _filterBar(context),
        if (_searching && _search.text.trim().runes.length >= 2)
          _messageMatches(context),
        if (buckets.isEmpty)
          Padding(
            key: const Key('chats-empty'),
            padding: const EdgeInsets.only(top: 72),
            child: Column(
              children: [
                Icon(
                  _search.text.isNotEmpty
                      ? Icons.search_off
                      : Icons.chat_bubble_outline,
                  size: 48,
                  color: Theme.of(context).colorScheme.onSurfaceVariant,
                ),
                const SizedBox(height: 12),
                Text(
                  _search.text.isNotEmpty
                      ? 'No chat matches the search.'
                      : 'No chats in this view yet.',
                ),
              ],
            ),
          )
        else
          for (final b in buckets) ...[
            if (b.folder != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 14, 16, 4),
                child: Row(children: [
                  Icon(
                    b.folder!.isSection
                        ? Icons.folder_outlined
                        : Icons.folder_open,
                    size: 18,
                    color: Theme.of(context).colorScheme.primary,
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      b.folder!.displayTitle,
                      style: Theme.of(context).textTheme.titleSmall,
                    ),
                  ),
                  if (b.unread > 0) Text('${b.unread}'),
                ]),
              ),
            for (final c in b.chats)
              ConversationTile(
                app: app,
                c: c,
                onTap: () => _open(c),
                onLongPress: () => _actions(c),
              ),
          ],
      ],
    );
  }

  String _hitTitle(ConversationSearchHit hit) {
    if (hit.title.isNotEmpty) return hit.title;
    for (final c in app.conversations) {
      if (c.id == hit.conversationId) return smartConversationTitle(c);
    }
    return 'Chat';
  }

  /// Message matches from the server full-text search (0.74.2): snippet
  /// previews that jump straight to the matching chat, like the desktop
  /// History popover. Shown under the instant title filter while searching.
  Widget _messageMatches(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  'Message matches',
                  style: Theme.of(context).textTheme.titleSmall,
                ),
              ),
              if (_searchingServer)
                const SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
            ],
          ),
        ),
        if (_searchError != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12),
            child: ErrorBanner(_searchError!),
          )
        else if (!_searchingServer && _hits.isEmpty)
          const Padding(
            padding: EdgeInsets.fromLTRB(16, 2, 16, 8),
            child: Text('No messages match that search.'),
          )
        else
          for (final hit in _hits)
            Card(
              key: Key('message-match-${hit.conversationId}'),
              margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 3),
              elevation: 0,
              color: scheme.surfaceContainerLow,
              shape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(14),
              ),
              child: ListTile(
                shape: RoundedRectangleBorder(
                  borderRadius: BorderRadius.circular(14),
                ),
                leading: const Icon(Icons.message_outlined),
                title: Text(
                  _hitTitle(hit),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                subtitle: hit.snippet.isEmpty
                    ? null
                    : Text(
                        hit.snippet,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                      ),
                trailing: hit.role.isEmpty
                    ? null
                    : Text(
                        hit.role == 'user' ? 'You' : 'Assistant',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                onTap: () => _openHit(hit),
              ),
            ),
        const SizedBox(height: 4),
        const Divider(),
      ],
    );
  }

  Widget _botQuickStrip(BuildContext context, List<BotSummary> bots) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox(
      height: 86,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
        itemCount: bots.length,
        separatorBuilder: (context, _) => const SizedBox(width: 12),
        itemBuilder: (context, i) {
          final b = bots[i];
          return InkWell(
            key: Key('bot-launch-${b.id}'),
            borderRadius: BorderRadius.circular(12),
            onTap: () => _launchBot(b),
            child: SizedBox(
              width: 64,
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Stack(
                    clipBehavior: Clip.none,
                    children: [
                      SeraAvatar(
                        label: b.name,
                        emoji: b.avatar,
                        app: app,
                        kind: 'bots',
                        id: b.id,
                        version: b.avatarVersion,
                        radius: 22,
                      ),
                      if (b.live)
                        Positioned(
                          right: -1,
                          bottom: -1,
                          child: Container(
                            width: 12,
                            height: 12,
                            decoration: BoxDecoration(
                              color: const Color(0xFF22C55E),
                              shape: BoxShape.circle,
                              border: Border.all(
                                color: scheme.surface,
                                width: 2,
                              ),
                            ),
                          ),
                        ),
                    ],
                  ),
                  const SizedBox(height: 5),
                  Text(
                    b.name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(context).textTheme.labelSmall,
                    textAlign: TextAlign.center,
                  ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }

  Widget _filterBar(BuildContext context) {
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
      child: Row(
        children: [
          _chip(ChatFilter.all, 'All'),
          const SizedBox(width: 8),
          _chip(ChatFilter.bots, 'Bots'),
          const SizedBox(width: 8),
          _chip(ChatFilter.channels, 'Channels'),
          const SizedBox(width: 8),
          _chip(ChatFilter.unread, 'Unread'),
        ],
      ),
    );
  }

  Widget _chip(ChatFilter f, String label) {
    return ChoiceChip(
      selected: _filter == f,
      label: Text(label),
      onSelected: (_) => setState(() => _filter = f),
      visualDensity: VisualDensity.compact,
    );
  }
}

class ConversationTile extends StatelessWidget {
  const ConversationTile({
    super.key,
    required this.app,
    required this.c,
    required this.onTap,
    this.onLongPress,
  });

  final AppState app;
  final ConversationSummary c;
  final VoidCallback onTap;
  final VoidCallback? onLongPress;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final unread = c.unreadCount;
    final last = c.lastMessage;
    final when = shortTime(c.lastMessageAt ?? c.updatedAt);
    final title = smartConversationTitle(c);
    final preview = last == null
        ? ''
        : (last.role == 'user' ? 'You: ${last.text}' : last.text);
    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 3),
      elevation: 0,
      color: unread > 0
          ? theme.colorScheme.primaryContainer.withValues(alpha: 0.18)
          : theme.colorScheme.surfaceContainerLow,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ListTile(
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
        onTap: onTap,
        onLongPress: onLongPress,
        leading: SeraAvatar(
          label: title,
          app: app,
          kind: 'conversations',
          id: c.id,
          version: c.avatarVersion,
          emoji: c.isRoom && c.avatarVersion == null ? '\u{1F465}' : null,
          radius: 22,
        ),
        title: Text(
          title,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: unread > 0 ? const TextStyle(fontWeight: FontWeight.w700) : null,
        ),
        subtitle: preview.isEmpty
            ? null
            : Text(
                preview.replaceAll('\n', ' '),
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
        trailing: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Text(when, style: theme.textTheme.bodySmall),
            if (unread > 0)
              Container(
                margin: const EdgeInsets.only(top: 4),
                padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
                decoration: BoxDecoration(
                  color: theme.colorScheme.primary,
                  borderRadius: BorderRadius.circular(10),
                ),
                child: Text(
                  unread > 99 ? '99+' : '$unread',
                  style: TextStyle(
                    color: theme.colorScheme.onPrimary,
                    fontSize: 12,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// Bot picker for a new chat or channel.
class NewChatSheet extends StatefulWidget {
  const NewChatSheet({super.key, required this.app, this.channelMode = false});
  final AppState app;
  final bool channelMode;

  @override
  State<NewChatSheet> createState() => _NewChatSheetState();
}

class _NewChatSheetState extends State<NewChatSheet> {
  final _name = TextEditingController();
  final _picked = <String>{};
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  Future<void> _create() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final client = widget.app.client;
      final name = _name.text.trim();
      final ConversationSummary created;
      if (_picked.isEmpty && !widget.channelMode) {
        created = await client.createChat(
          kind: ConversationKind.direct,
          slug: 'chat-${DateTime.now().millisecondsSinceEpoch.toRadixString(36)}',
          title: name.isEmpty ? 'New chat' : name,
        );
      } else {
        final fallback = _picked.length == 1
            ? (widget.app.botById(_picked.first)?.name ?? _picked.first)
            : (widget.channelMode ? 'New channel' : 'Group chat');
        created = await client.createGroupChat(
          name.isEmpty ? fallback : name,
          _picked.toList(),
        );
      }
      if (mounted) {
        Navigator.of(context).pop(created);
      }
    } on ApiException catch (e) {
      setState(() {
        _error = e.messageForUser();
        _busy = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final bots = widget.app.bots.where((b) => b.enabled).toList();
    return Padding(
      padding: EdgeInsets.only(
        left: 16,
        right: 16,
        bottom: MediaQuery.of(context).viewInsets.bottom + 16,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            widget.channelMode ? 'New channel' : 'New chat',
            style: Theme.of(context).textTheme.titleLarge,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _name,
            decoration: InputDecoration(
              labelText: widget.channelMode ? 'Channel name' : 'Name (optional)',
            ),
            maxLength: 60,
          ),
          Text('Talk to', style: Theme.of(context).textTheme.titleSmall),
          const SizedBox(height: 4),
          Flexible(
            child: ListView(
              shrinkWrap: true,
              children: [
                if (bots.isEmpty)
                  const Text(
                      'No bots loaded. The chat uses the default assistant.'),
                for (final b in bots)
                  CheckboxListTile(
                    value: _picked.contains(b.id),
                    onChanged: _busy
                        ? null
                        : (v) => setState(() {
                              v == true
                                  ? _picked.add(b.id)
                                  : _picked.remove(b.id);
                            }),
                    secondary: SeraAvatar(
                      label: b.name,
                      emoji: b.avatar,
                      app: widget.app,
                      id: b.id,
                      version: b.avatarVersion,
                    ),
                    title: Text(b.name),
                    subtitle: b.description == null || b.description!.isEmpty
                        ? null
                        : Text(b.description!,
                            maxLines: 1, overflow: TextOverflow.ellipsis),
                  ),
              ],
            ),
          ),
          if (_error != null) ...[
            const SizedBox(height: 8),
            ErrorBanner(_error!),
          ],
          const SizedBox(height: 8),
          SizedBox(
            width: double.infinity,
            child: FilledButton(
              onPressed: _busy ? null : _create,
              child: Text(
                _picked.isEmpty
                    ? (widget.channelMode ? 'Create channel' : 'Start chat')
                    : 'Start group with ${_picked.length}',
              ),
            ),
          ),
        ],
      ),
    );
  }
}
