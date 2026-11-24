import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../bot_library_models.dart';
import '../chat_models.dart';
import '../device.dart';
import '../preview_pane.dart';
import '../usage_insights_models.dart';
import '../widgets.dart';
import 'bot_library_screen.dart';

/// Member's cross-chat Library (`GET /v1/library`, `POST /v1/library/upload`):
/// every file made by bots across chats plus direct uploads, with tabs
/// (All / Made by bots / Uploaded), search bar, Grid/List toggle, authenticated
/// thumbnails + full-screen `InteractiveViewer`, PPTX/XLSX preview sheet, PDF
/// preview, Share, and upload via `file_picker` showing OneDrive mirror status.
class LibraryScreen extends StatefulWidget {
  const LibraryScreen({
    super.key,
    required this.app,
    this.device,
  });

  final AppState app;
  final DeviceActions? device;

  @override
  State<LibraryScreen> createState() => _LibraryScreenState();
}

enum _LibraryViewMode { list, grid }

const _globalTabs = <LibraryKind>[
  LibraryKind.all,
  LibraryKind.madeByBots,
  LibraryKind.uploaded,
];

class _LibraryScreenState extends State<LibraryScreen> {
  final _search = TextEditingController();
  bool _loading = true;
  bool _uploading = false;
  String? _error;
  String? _uploadNotice;
  List<BotLibraryItem> _items = const [];
  LibraryOnedriveStatus _onedrive = const LibraryOnedriveStatus();
  LibraryKind _kind = LibraryKind.all;
  _LibraryViewMode _viewMode = _LibraryViewMode.list;
  String _query = '';
  String _today = todayUtcDay();
  late final DeviceActions _device;

  @override
  void initState() {
    super.initState();
    _device = widget.device ?? PlatformDeviceActions();
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
      final resp = await widget.app.client.getGlobalLibrary();
      if (!mounted) {
        return;
      }
      setState(() {
        _items = resp.items;
        _onedrive = resp.onedrive;
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

  Future<void> _upload() async {
    if (_uploading) {
      return;
    }
    final messenger = ScaffoldMessenger.of(context);
    messenger.hideCurrentSnackBar();
    final PickedFile? picked;
    try {
      picked = await _device.pickAnyFile(maxBytes: maxAttachmentBytes);
    } on FormatException {
      if (mounted) {
        messenger.showSnackBar(
          const SnackBar(content: Text('Files must be 25 MB or smaller.')),
        );
      }
      return;
    } catch (_) {
      if (mounted) {
        messenger.showSnackBar(
          const SnackBar(content: Text('Could not read the chosen file.')),
        );
      }
      return;
    }
    if (picked == null || !mounted) {
      return;
    }
    if (picked.bytes.isEmpty) {
      messenger.showSnackBar(
        const SnackBar(content: Text('That file is empty.')),
      );
      return;
    }
    if (picked.bytes.length > maxAttachmentBytes) {
      messenger.showSnackBar(
        const SnackBar(content: Text('Files must be 25 MB or smaller.')),
      );
      return;
    }
    setState(() {
      _uploading = true;
      _uploadNotice = null;
    });
    try {
      final reply = await widget.app.client.uploadToLibrary(
        picked.name,
        picked.bytes,
      );
      if (!mounted) {
        return;
      }
      final summary = reply.onedriveSummary;
      setState(() {
        _uploading = false;
        _uploadNotice = summary;
      });
      messenger.hideCurrentSnackBar();
      messenger.showSnackBar(SnackBar(content: Text(summary)));
      await _load();
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _uploading = false);
      messenger.hideCurrentSnackBar();
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  void _clearSearch() {
    _search.clear();
    setState(() => _query = '');
  }

  List<BotLibraryItem> get _visible =>
      libraryView(_items, kind: _kind, query: _query);

  Future<void> _shareItem(BotLibraryItem item) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      final bytes = await widget.app.client.libraryItemBytes(item);
      await _device.shareFile(item.downloadName, bytes);
    } on ApiException catch (e) {
      if (mounted) {
        messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    } catch (_) {
      if (mounted) {
        messenger.showSnackBar(
          const SnackBar(content: Text('Could not open the file.')),
        );
      }
    }
  }

  void _openItem(BotLibraryItem item) {
    if (item.isImage) {
      _openImageViewer(item);
      return;
    }
    if (item.docPreviewKind != null && item.isArtifactHash) {
      showLibraryDocPreviewSheet(
        context,
        client: widget.app.client,
        item: item,
        onOpenFile: () => _shareItem(item),
      );
      return;
    }
    if (item.isPdf && item.isArtifactHash) {
      _openPdf(item);
      return;
    }
    _showDetails(item);
  }

  void _openImageViewer(BotLibraryItem item) {
    final images = _visible.where((i) => i.isImage).toList();
    var start = images.indexWhere((i) => i.id == item.id);
    if (start < 0) {
      start = 0;
    }
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        fullscreenDialog: true,
        builder: (_) => LibraryImageViewer(
          client: widget.app.client,
          device: _device,
          items: images,
          startIndex: start,
        ),
      ),
    );
  }

  void _openPdf(BotLibraryItem item) {
    final entry = LibraryEntry(
      actionId: item.id,
      tool: item.tool,
      hash: item.id,
      filename: item.filename,
      title: item.label,
      mime: item.mime,
      createdAt: item.createdAt,
    );
    showArtifactPreviewSheet(
      context,
      client: widget.app.client,
      entry: entry,
      onFallback: () {
        Navigator.pop(context);
        _shareItem(item);
      },
    );
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
                if (item.filename != null)
                  _DetailRow(label: 'File', value: item.filename!),
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
                const SizedBox(height: 12),
                FilledButton.icon(
                  onPressed: () async {
                    Navigator.of(ctx).pop();
                    await _shareItem(item);
                  },
                  icon: const Icon(Icons.share_outlined),
                  label: const Text('Download / share'),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    final visible = _visible;
    final groups = groupLibraryItems(visible, _today);
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Library'),
        actions: [
          IconButton(
            key: const Key('global-library-view-toggle'),
            tooltip: _viewMode == _LibraryViewMode.list
                ? 'Grid view'
                : 'List view',
            onPressed: () => setState(() {
              _viewMode = _viewMode == _LibraryViewMode.list
                  ? _LibraryViewMode.grid
                  : _LibraryViewMode.list;
            }),
            icon: Icon(
              _viewMode == _LibraryViewMode.list
                  ? Icons.grid_view_outlined
                  : Icons.view_list_outlined,
            ),
          ),
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        key: const Key('global-library-upload'),
        heroTag: null,
        onPressed: _uploading ? null : _upload,
        icon: _uploading
            ? const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : const Icon(Icons.upload_file),
        label: Text(_uploading ? 'Uploading…' : 'Upload'),
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 96),
          children: [
            TextField(
              key: const Key('global-library-search'),
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
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                for (final kind in _globalTabs)
                  ChoiceChip(
                    key: Key('global-library-tab-${kind.name}'),
                    label: Text(kind.label),
                    selected: _kind == kind,
                    onSelected: (_) => setState(() => _kind = kind),
                  ),
              ],
            ),
            if (!_loading) ...[
              const SizedBox(height: 6),
              Row(
                children: [
                  Icon(
                    _onedrive.connected
                        ? Icons.cloud_done_outlined
                        : Icons.cloud_off_outlined,
                    size: 16,
                    color: scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      _onedrive.statusLabel,
                      key: const Key('global-library-onedrive-status'),
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                ],
              ),
            ],
            if (_uploadNotice != null) ...[
              const SizedBox(height: 8),
              Material(
                key: const Key('global-library-upload-notice'),
                color: scheme.secondaryContainer,
                borderRadius: BorderRadius.circular(12),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 10,
                  ),
                  child: Row(
                    children: [
                      Icon(
                        Icons.cloud_upload_outlined,
                        size: 18,
                        color: scheme.onSecondaryContainer,
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          _uploadNotice!,
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: scheme.onSecondaryContainer,
                          ),
                        ),
                      ),
                      IconButton(
                        tooltip: 'Dismiss',
                        visualDensity: VisualDensity.compact,
                        onPressed: () => setState(() => _uploadNotice = null),
                        icon: Icon(
                          Icons.close,
                          size: 16,
                          color: scheme.onSecondaryContainer,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ],
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
                    'Nothing in your library yet. Files your bots make and '
                    'files you upload show here.',
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
            else if (_viewMode == _LibraryViewMode.grid)
              for (final group in groups) ...[
                Padding(
                  padding: const EdgeInsets.fromLTRB(4, 12, 4, 8),
                  child: Text(group.heading, style: theme.textTheme.titleSmall),
                ),
                GridView.builder(
                  shrinkWrap: true,
                  physics: const NeverScrollableScrollPhysics(),
                  itemCount: group.items.length,
                  gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
                    crossAxisCount: 2,
                    mainAxisSpacing: 8,
                    crossAxisSpacing: 8,
                    childAspectRatio: 0.85,
                  ),
                  itemBuilder: (context, idx) {
                    final item = group.items[idx];
                    return _GlobalLibraryGridCard(
                      client: widget.app.client,
                      item: item,
                      onTap: () => _openItem(item),
                      onShare: () => _shareItem(item),
                    );
                  },
                ),
              ]
            else
              for (final group in groups) ...[
                Padding(
                  padding: const EdgeInsets.fromLTRB(4, 12, 4, 4),
                  child: Text(group.heading, style: theme.textTheme.titleSmall),
                ),
                for (final item in group.items)
                  _GlobalLibraryListTile(
                    client: widget.app.client,
                    item: item,
                    onTap: () => _openItem(item),
                    onShare: () => _shareItem(item),
                  ),
              ],
          ],
        ),
      ),
    );
  }
}

class _GlobalLibraryListTile extends StatelessWidget {
  const _GlobalLibraryListTile({
    required this.client,
    required this.item,
    required this.onTap,
    required this.onShare,
  });

  final ApiClient client;
  final BotLibraryItem item;
  final VoidCallback onTap;
  final VoidCallback onShare;

  @override
  Widget build(BuildContext context) {
    final icon = item.isMedia
        ? Icons.image_outlined
        : item.generated
        ? Icons.auto_awesome_outlined
        : Icons.description_outlined;
    final filename = item.filename;
    final detailLine = [
      if (filename != null && filename != item.label) filename,
      if (item.sizeBytes != null) humanBytes(item.sizeBytes!),
      item.day,
    ].join(' · ');
    final metaLine = [
      item.kindLabel,
      if (item.conversationTitle != null) item.conversationTitle!,
    ].join(' · ');
    return ListTile(
      key: ValueKey('global-library-${item.id}'),
      onTap: onTap,
      contentPadding: const EdgeInsets.symmetric(horizontal: 4),
      leading: item.isImage
          ? LibraryThumbnail(client: client, item: item)
          : Icon(icon),
      title: Text(item.label, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(detailLine, maxLines: 1, overflow: TextOverflow.ellipsis),
          Text(metaLine, maxLines: 1, overflow: TextOverflow.ellipsis),
        ],
      ),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          LibraryTypeBadge(item: item),
          IconButton(
            tooltip: 'Share',
            icon: const Icon(Icons.share_outlined, size: 20),
            onPressed: onShare,
          ),
        ],
      ),
    );
  }
}

class _GlobalLibraryGridCard extends StatelessWidget {
  const _GlobalLibraryGridCard({
    required this.client,
    required this.item,
    required this.onTap,
    required this.onShare,
  });

  final ApiClient client;
  final BotLibraryItem item;
  final VoidCallback onTap;
  final VoidCallback onShare;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Card(
      key: ValueKey('global-library-grid-${item.id}'),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: onTap,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Expanded(
              child: ColoredBox(
                color: scheme.surfaceContainerHighest,
                child: item.isImage
                    ? LibraryThumbnail(
                        client: client,
                        item: item,
                        size: double.infinity,
                        fit: BoxFit.cover,
                      )
                    : Center(
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Icon(
                              item.generated
                                  ? Icons.auto_awesome_outlined
                                  : Icons.description_outlined,
                              size: 30,
                              color: scheme.onSurfaceVariant,
                            ),
                            const SizedBox(height: 6),
                            LibraryTypeBadge(item: item),
                          ],
                        ),
                      ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(10, 8, 6, 8),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          item.label,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.labelLarge,
                        ),
                        Text(
                          [
                            item.kindLabel,
                            if (item.sizeBytes != null)
                              humanBytes(item.sizeBytes!),
                          ].join(' · '),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ],
                    ),
                  ),
                  IconButton(
                    tooltip: 'Share',
                    visualDensity: VisualDensity.compact,
                    icon: const Icon(Icons.share_outlined, size: 18),
                    onPressed: onShare,
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
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
