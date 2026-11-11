import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../device.dart';
import '../models.dart';
import '../widgets.dart';
import 'chat_screen.dart';

/// The same conversations as the desktop rail, in the server's order, with
/// sections and groups, search, unread counts and a new-chat sheet.
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

  AppState get app => widget.app;

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  /// Long-press menu: Rename or Archive. Archive is what the server's delete
  /// does (the chat can be restored), so it asks first and offers Undo.
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
              key: const Key('chat-archive'),
              leading: const Icon(Icons.archive_outlined),
              title: const Text('Archive'),
              onTap: () => Navigator.of(sheet).pop('archive'),
            ),
          ],
        ),
      ),
    );
    if (!mounted) {
      return;
    }
    if (choice == 'rename') {
      await _rename(c);
    } else if (choice == 'archive') {
      await _archive(c);
    }
  }

  Future<void> _rename(ConversationSummary c) async {
    final controller = TextEditingController(text: c.displayTitle);
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
    if (next.isEmpty || next == c.displayTitle || !mounted) {
      return;
    }
    final messenger = ScaffoldMessenger.of(context);
    try {
      await app.client.renameConversation(c.id, next);
      await app.refreshConversations();
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  Future<void> _archive(ConversationSummary c) async {
    final ok = await confirmAction(
      context,
      title: 'Archive this chat?',
      message: '"${c.displayTitle}" leaves your list. You can undo right after.',
      confirmLabel: 'Archive',
    );
    if (!ok || !mounted) {
      return;
    }
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
      builder: (_) => ChatScreen(app: app, conversation: c, device: widget.device),
    ));
  }

  Future<void> _newChat() async {
    final created = await showModalBottomSheet<ConversationSummary>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => NewChatSheet(app: app),
    );
    if (created != null && mounted) {
      await app.refreshConversations();
      if (mounted) {
        _open(created);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        final buckets = bucketConversations(app.conversations, query: _search.text);
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
                    onChanged: (_) => setState(() {}),
                  )
                : const Text('Chats'),
            actions: [
              IconButton(
                tooltip: _searching ? 'Close search' : 'Search',
                icon: Icon(_searching ? Icons.close : Icons.search),
                onPressed: () => setState(() {
                  _searching = !_searching;
                  if (!_searching) {
                    _search.clear();
                  }
                }),
              ),
            ],
          ),
          floatingActionButton: FloatingActionButton.extended(
            heroTag: null,
            onPressed: _newChat,
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
    if (app.conversationsError != null && app.conversations.isEmpty) {
      return ListView(physics: const AlwaysScrollableScrollPhysics(), padding: const EdgeInsets.all(16), children: [
        ErrorBanner(app.conversationsError!, onRetry: app.refreshConversations),
      ]);
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
    if (buckets.isEmpty) {
      final searching = _search.text.isNotEmpty;
      return ListView(
        key: const Key('chats-empty'),
        physics: const AlwaysScrollableScrollPhysics(),
        children: [
          const SizedBox(height: 120),
          Icon(
            searching ? Icons.search_off : Icons.chat_bubble_outline,
            size: 48,
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
          const SizedBox(height: 12),
          Center(
            child: Text(searching
                ? 'No chat matches the search.'
                : 'No chats yet. Start one below.'),
          ),
          if (!searching)
            const Padding(
              padding: EdgeInsets.only(top: 4),
              child: Center(child: Text('Pull down to refresh.')),
            ),
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
            child: ErrorBanner(app.conversationsError!, onRetry: app.refreshConversations),
          ),
        for (final b in buckets) ...[
          if (b.folder != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
              child: Row(children: [
                Icon(b.folder!.isSection ? Icons.folder_outlined : Icons.folder_open,
                    size: 18, color: Theme.of(context).colorScheme.primary),
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
    final preview = last == null
        ? ''
        : (last.role == 'user' ? 'You: ${last.text}' : last.text);
    return ListTile(
      onTap: onTap,
      onLongPress: onLongPress,
      leading: SeraAvatar(
        label: c.displayTitle,
        app: app,
        kind: 'conversations',
        id: c.id,
        version: c.avatarVersion,
        emoji: c.isRoom && c.avatarVersion == null ? '\u{1F465}' : null,
      ),
      title: Text(
        c.displayTitle,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: unread > 0 ? const TextStyle(fontWeight: FontWeight.w700) : null,
      ),
      subtitle: preview.isEmpty
          ? null
          : Text(preview.replaceAll('\n', ' '), maxLines: 1, overflow: TextOverflow.ellipsis),
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
                style: TextStyle(color: theme.colorScheme.onPrimary, fontSize: 12),
              ),
            ),
        ],
      ),
    );
  }
}

/// Bot picker for a new chat: no bot starts a normal chat; one or more bots
/// create a group chat with them as its team (`POST /v1/groups`).
class NewChatSheet extends StatefulWidget {
  const NewChatSheet({super.key, required this.app});
  final AppState app;

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
      if (_picked.isEmpty) {
        created = await client.createChat(
          kind: ConversationKind.direct,
          slug: 'chat-${DateTime.now().millisecondsSinceEpoch.toRadixString(36)}',
          title: name.isEmpty ? 'New chat' : name,
        );
      } else {
        final fallback = _picked.length == 1
            ? (widget.app.botById(_picked.first)?.name ?? _picked.first)
            : 'Group chat';
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
          Text('New chat', style: Theme.of(context).textTheme.titleLarge),
          const SizedBox(height: 12),
          TextField(
            controller: _name,
            decoration: const InputDecoration(labelText: 'Name (optional)'),
            maxLength: 60,
          ),
          Text('Talk to', style: Theme.of(context).textTheme.titleSmall),
          const SizedBox(height: 4),
          Flexible(
            child: ListView(
              shrinkWrap: true,
              children: [
                if (bots.isEmpty) const Text('No bots loaded. The chat uses the default assistant.'),
                for (final b in bots)
                  CheckboxListTile(
                    value: _picked.contains(b.id),
                    onChanged: _busy
                        ? null
                        : (v) => setState(() {
                              v == true ? _picked.add(b.id) : _picked.remove(b.id);
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
                        : Text(b.description!, maxLines: 1, overflow: TextOverflow.ellipsis),
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
              child: Text(_picked.isEmpty ? 'Start chat' : 'Start group with ${_picked.length}'),
            ),
          ),
        ],
      ),
    );
  }
}
