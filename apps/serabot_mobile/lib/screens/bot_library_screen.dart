import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../bot_library_models.dart';
import '../usage_insights_models.dart';
import '../widgets.dart';

/// A bot's library (`GET /v1/bots/{bot}/library`, 0.63): the files its bots
/// made and the files the member uploaded in its chats. Filters, search and
/// date groups mirror the desktop library. Tap a file for its details.
class BotLibraryScreen extends StatefulWidget {
  const BotLibraryScreen({
    super.key,
    required this.app,
    required this.botId,
    required this.botName,
  });

  final AppState app;
  final String botId;
  final String botName;

  @override
  State<BotLibraryScreen> createState() => _BotLibraryScreenState();
}

class _BotLibraryScreenState extends State<BotLibraryScreen> {
  final _search = TextEditingController();
  bool _loading = true;
  String? _error;
  List<BotLibraryItem> _items = const [];
  LibraryKind _kind = LibraryKind.all;
  String _query = '';
  String _today = todayUtcDay();

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final items = await widget.app.client.botLibrary(widget.botId);
      if (!mounted) {
        return;
      }
      setState(() {
        _items = items;
        _today = todayUtcDay();
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  void _clearSearch() {
    _search.clear();
    setState(() => _query = '');
  }

  void _showDetails(BotLibraryItem item) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (ctx) {
        final theme = Theme.of(ctx);
        final ext = item.extension;
        return SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 16),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(item.label, style: theme.textTheme.titleMedium),
                const SizedBox(height: 12),
                _DetailRow(label: 'Kind', value: item.kindLabel),
                _DetailRow(
                  label: 'Type',
                  value: ext.isNotEmpty
                      ? ext.toUpperCase()
                      : (item.mime.isEmpty ? 'Unknown' : item.mime),
                ),
                _DetailRow(
                  label: 'Size',
                  value: item.sizeBytes == null
                      ? 'Not recorded'
                      : humanBytes(item.sizeBytes!),
                ),
                _DetailRow(label: 'Date', value: item.day),
                _DetailRow(label: 'From', value: item.origin),
              ],
            ),
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    final visible = libraryView(_items, kind: _kind, query: _query);
    final groups = groupLibraryItems(visible, _today);
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Text('${widget.botName} library'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 24),
          children: [
            TextField(
              key: const Key('library-search'),
              controller: _search,
              onChanged: (v) => setState(() => _query = v),
              decoration: InputDecoration(
                prefixIcon: const Icon(Icons.search),
                hintText: 'Search files and chats',
                suffixIcon: _query.isEmpty
                    ? null
                    : IconButton(
                        tooltip: 'Clear search',
                        onPressed: _clearSearch,
                        icon: const Icon(Icons.clear),
                      ),
              ),
            ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              runSpacing: 4,
              children: [
                for (final kind in LibraryKind.values)
                  ChoiceChip(
                    label: Text(kind.label),
                    selected: _kind == kind,
                    onSelected: (_) => setState(() => _kind = kind),
                  ),
              ],
            ),
            const SizedBox(height: 8),
            if (_error != null) ...[
              ErrorBanner(_error!, onRetry: _load),
              const SizedBox(height: 8),
            ],
            if (_loading && _items.isEmpty)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 48),
                child: Center(child: CircularProgressIndicator()),
              )
            else if (_items.isEmpty && _error == null)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 48),
                child: Center(
                  child: Text(
                    'Nothing in this bot’s chats yet. Files its bots make '
                    'and files you upload show here.',
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium,
                  ),
                ),
              )
            else if (visible.isEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 32),
                child: Center(
                  child: Text(
                    'No files match.',
                    style: theme.textTheme.bodyMedium,
                  ),
                ),
              )
            else
              for (final group in groups) ...[
                Padding(
                  padding: const EdgeInsets.fromLTRB(4, 12, 4, 4),
                  child: Text(group.heading, style: theme.textTheme.titleSmall),
                ),
                for (final item in group.items)
                  _LibraryTile(item: item, onTap: () => _showDetails(item)),
              ],
          ],
        ),
      ),
    );
  }
}

class _LibraryTile extends StatelessWidget {
  const _LibraryTile({required this.item, required this.onTap});

  final BotLibraryItem item;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final icon = item.isMedia
        ? Icons.image_outlined
        : item.generated
        ? Icons.auto_awesome_outlined
        : Icons.description_outlined;
    final ext = item.extension;
    return ListTile(
      key: ValueKey('library-${item.id}'),
      onTap: onTap,
      contentPadding: const EdgeInsets.symmetric(horizontal: 4),
      leading: Icon(icon),
      title: Text(item.label, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Text(
        [
          item.kindLabel,
          if (item.conversationTitle != null) item.conversationTitle!,
          if (ext.isNotEmpty) ext.toUpperCase(),
        ].join(' · '),
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: Text(item.day, style: Theme.of(context).textTheme.labelSmall),
    );
  }
}

class _DetailRow extends StatelessWidget {
  const _DetailRow({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 72,
            child: Text(label, style: theme.textTheme.bodySmall),
          ),
          Expanded(child: Text(value)),
        ],
      ),
    );
  }
}
