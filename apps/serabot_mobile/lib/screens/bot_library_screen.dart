import 'dart:typed_data';

import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../bot_library_models.dart';
import '../chat_models.dart';
import '../device.dart';
import '../preview_pane.dart';
import '../usage_insights_models.dart';
import '../widgets.dart';

/// A bot's library (`GET /v1/bots/{bot}/library`, 0.63): the files its bots
/// made and the files the member uploaded in its chats. Filters, search and
/// date groups mirror the desktop library.
///
/// 0.74.9: image items show real thumbnails (from the authenticated
/// artifact preview, never an unauthenticated URL); tapping an image opens
/// a full-screen viewer with pinch zoom and prev/next; tapping a PPTX/XLSX
/// opens a structured preview (`GET /v1/library/preview-doc`, with a
/// friendly "open the file instead" state on servers without it); PDFs
/// reuse the existing artifact preview pane. Download/share goes through
/// the system share sheet — there is no `url_launcher`/`open_file`
/// dependency, so "open with the default app" is the share sheet's job.
class BotLibraryScreen extends StatefulWidget {
  const BotLibraryScreen({
    super.key,
    required this.app,
    required this.botId,
    required this.botName,
    this.device,
  });

  final AppState app;
  final String botId;
  final String botName;

  /// Share/download target. Defaults to the platform implementation when
  /// the caller does not pass one (widget tests).
  final DeviceActions? device;

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

  /// The items currently visible under the active filter/search, in
  /// display order. The image viewer steps through this list's images.
  List<BotLibraryItem> get _visible =>
      libraryView(_items, kind: _kind, query: _query);

  /// Download the item and open the system share sheet (the way to save
  /// the file or open it in another app). Errors are a snackbar, never a
  /// crash.
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
    if (item.docPreviewKind != null) {
      // Only generated items carry the artifact hash preview-doc needs;
      // an uploaded PPTX/XLSX falls through to details + share.
      if (item.isArtifactHash) {
        _showDocPreview(item);
        return;
      }
      _showDetails(item);
      return;
    }
    if (item.isPdf) {
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

  /// Structured PPTX/XLSX preview. A 404 (server without preview-doc) or
  /// an empty preview shows "open the file instead" with a share button;
  /// nothing here throws.
  Future<void> _showDocPreview(BotLibraryItem item) async {
    await showLibraryDocPreviewSheet(
      context,
      client: widget.app.client,
      item: item,
      onOpenFile: () => _shareItem(item),
    );
  }

  /// PDFs reuse the existing artifact preview pane (page-1 image plus
  /// extracted text). An uploaded PDF has no artifact hash, so it gets
  /// the details sheet with a share button instead.
  void _openPdf(BotLibraryItem item) {
    if (!item.isArtifactHash) {
      _showDetails(item);
      return;
    }
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
                  _LibraryTile(
                    client: widget.app.client,
                    item: item,
                    onTap: () => _openItem(item),
                  ),
              ],
          ],
        ),
      ),
    );
  }
}

class _LibraryTile extends StatelessWidget {
  const _LibraryTile({
    required this.client,
    required this.item,
    required this.onTap,
  });

  final ApiClient client;
  final BotLibraryItem item;
  final VoidCallback onTap;

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
      key: ValueKey('library-${item.id}'),
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
      trailing: LibraryTypeBadge(item: item),
    );
  }
}

/// Small type badge for a tile: the extension in caps, or a media/file
/// fallback when the name carries none.
class LibraryTypeBadge extends StatelessWidget {
  const LibraryTypeBadge({super.key, required this.item});

  final BotLibraryItem item;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final ext = item.extension;
    final label = ext.isNotEmpty
        ? ext.toUpperCase()
        : (item.isMedia ? 'MEDIA' : 'FILE');
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        border: Border.all(color: scheme.outlineVariant),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Text(
        label,
        style: Theme.of(
          context,
        ).textTheme.labelSmall?.copyWith(fontWeight: FontWeight.w700),
      ),
    );
  }
}

/// Real thumbnail for one image item, fetched once from the authenticated
/// client (thumbnail endpoint -> artifact preview -> download). Loading and
/// failure both show the plain image icon, never a broken tile.
class LibraryThumbnail extends StatefulWidget {
  const LibraryThumbnail({
    super.key,
    required this.client,
    required this.item,
    this.size = 56,
    this.fit = BoxFit.cover,
  });

  final ApiClient client;
  final BotLibraryItem item;
  final double size;
  final BoxFit fit;

  @override
  State<LibraryThumbnail> createState() => _LibraryThumbnailState();
}

class _LibraryThumbnailState extends State<LibraryThumbnail> {
  late final Future<List<int>?> _future;

  @override
  void initState() {
    super.initState();
    _future = widget.client.libraryThumbnailBytes(widget.item);
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<List<int>?>(
      future: _future,
      builder: (context, snap) {
        final bytes = snap.data;
        final Widget child;
        if (snap.connectionState == ConnectionState.done &&
            bytes != null &&
            bytes.isNotEmpty) {
          child = Image.memory(
            Uint8List.fromList(bytes),
            width: widget.size.isFinite ? widget.size : null,
            height: widget.size.isFinite ? widget.size : null,
            fit: widget.fit,
            cacheWidth: 320,
            errorBuilder: (_, _, _) => const Icon(Icons.broken_image_outlined),
          );
        } else {
          child = const Center(child: Icon(Icons.image_outlined));
        }
        if (!widget.size.isFinite) {
          return child;
        }
        return ClipRRect(
          borderRadius: BorderRadius.circular(8),
          child: SizedBox(width: widget.size, height: widget.size, child: child),
        );
      },
    );
  }
}

/// Opens the structured PPTX/XLSX preview modal (`GET /v1/library/preview-doc`).
Future<void> showLibraryDocPreviewSheet(
  BuildContext context, {
  required ApiClient client,
  required BotLibraryItem item,
  required VoidCallback onOpenFile,
}) async {
  final kind = item.docPreviewKind;
  if (kind == null || !item.isArtifactHash) {
    return;
  }
  final future = client.libraryDocPreview(item.id, kind);
  await showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) {
      final theme = Theme.of(ctx);
      return SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                item.label,
                style: theme.textTheme.titleMedium,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              const SizedBox(height: 4),
              Text(
                kind == 'pptx' ? 'Slides' : 'Sheets',
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 8),
              ConstrainedBox(
                constraints: BoxConstraints(
                  maxHeight: MediaQuery.of(ctx).size.height * 0.7,
                ),
                child: FutureBuilder<LibraryDocPreview?>(
                  future: future,
                  builder: (context, snap) {
                    if (snap.connectionState != ConnectionState.done) {
                      return const Padding(
                        padding: EdgeInsets.all(32),
                        child: Center(child: CircularProgressIndicator()),
                      );
                    }
                    final preview = snap.data;
                    if (snap.hasError || preview == null) {
                      return _DocPreviewUnavailable(
                        onOpenFile: () {
                          Navigator.of(ctx).pop();
                          onOpenFile();
                        },
                      );
                    }
                    if (kind == 'pptx') {
                      if (preview.slides.isEmpty) {
                        return _DocPreviewUnavailable(
                          message:
                              'This file has no slides to show — open the file instead.',
                          onOpenFile: () {
                            Navigator.of(ctx).pop();
                            onOpenFile();
                          },
                        );
                      }
                      return _DocPreviewSlides(slides: preview.slides);
                    }
                    if (preview.sheets.isEmpty) {
                      return _DocPreviewUnavailable(
                        message:
                            'This file has no sheets to show — open the file instead.',
                        onOpenFile: () {
                          Navigator.of(ctx).pop();
                          onOpenFile();
                        },
                      );
                    }
                    return _DocPreviewSheets(sheets: preview.sheets);
                  },
                ),
              ),
            ],
          ),
        ),
      );
    },
  );
}

/// Full-screen image viewer: pinch zoom, prev/next through the visible
/// images, share (system sheet) and close. Bytes are fetched per item
/// through the authenticated client and memoized per index.
class LibraryImageViewer extends StatefulWidget {
  const LibraryImageViewer({
    super.key,
    required this.client,
    required this.device,
    required this.items,
    required this.startIndex,
  });

  final ApiClient client;
  final DeviceActions device;
  final List<BotLibraryItem> items;
  final int startIndex;

  @override
  State<LibraryImageViewer> createState() => _LibraryImageViewerState();
}

class _LibraryImageViewerState extends State<LibraryImageViewer> {
  late int _index;
  final _futures = <int, Future<List<int>>>{};

  @override
  void initState() {
    super.initState();
    _index = widget.startIndex.clamp(0, widget.items.length - 1);
  }

  Future<List<int>> _bytesFor(int i) => _futures.putIfAbsent(
    i,
    () => widget.client.libraryItemBytes(widget.items[i]),
  );

  Future<void> _shareCurrent() async {
    final item = widget.items[_index];
    final messenger = ScaffoldMessenger.of(context);
    try {
      final bytes = await _bytesFor(_index);
      await widget.device.shareFile(item.downloadName, bytes);
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

  void _step(int delta) {
    final next = _index + delta;
    if (next < 0 || next >= widget.items.length) {
      return;
    }
    setState(() => _index = next);
  }

  @override
  Widget build(BuildContext context) {
    final item = widget.items[_index];
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Text(item.label, maxLines: 1, overflow: TextOverflow.ellipsis),
        actions: [
          IconButton(
            tooltip: 'Share / save',
            icon: const Icon(Icons.share_outlined),
            onPressed: _shareCurrent,
          ),
        ],
      ),
      body: Column(
        children: [
          Expanded(
            child: FutureBuilder<List<int>>(
              future: _bytesFor(_index),
              builder: (context, snap) {
                if (snap.connectionState != ConnectionState.done) {
                  return const Center(child: CircularProgressIndicator());
                }
                if (snap.hasError || snap.data == null) {
                  return Center(
                    child: Padding(
                      padding: const EdgeInsets.all(24),
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          const Icon(Icons.broken_image_outlined, size: 40),
                          const SizedBox(height: 8),
                          Text(
                            'Could not load this image.',
                            style: theme.textTheme.bodyMedium,
                            textAlign: TextAlign.center,
                          ),
                          const SizedBox(height: 12),
                          FilledButton(
                            onPressed: () {
                              setState(() => _futures.remove(_index));
                            },
                            child: const Text('Retry'),
                          ),
                        ],
                      ),
                    ),
                  );
                }
                return Container(
                  color: const Color(0xFF111318),
                  child: Center(
                    child: InteractiveViewer(
                      minScale: 0.5,
                      maxScale: 6,
                      child: Image.memory(
                        Uint8List.fromList(snap.data!),
                        fit: BoxFit.contain,
                        errorBuilder: (_, _, _) => const Icon(
                          Icons.broken_image_outlined,
                          size: 40,
                          color: Colors.white70,
                        ),
                      ),
                    ),
                  ),
                );
              },
            ),
          ),
          SafeArea(
            top: false,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
              child: Row(
                children: [
                  IconButton(
                    tooltip: 'Previous image',
                    icon: const Icon(Icons.chevron_left),
                    onPressed: _index > 0 ? () => _step(-1) : null,
                  ),
                  Expanded(
                    child: Text(
                      '${_index + 1} of ${widget.items.length}',
                      textAlign: TextAlign.center,
                      style: theme.textTheme.bodyMedium,
                    ),
                  ),
                  IconButton(
                    tooltip: 'Next image',
                    icon: const Icon(Icons.chevron_right),
                    onPressed: _index < widget.items.length - 1
                        ? () => _step(1)
                        : null,
                  ),
                  FilledButton.tonalIcon(
                    onPressed: _shareCurrent,
                    icon: const Icon(Icons.share_outlined, size: 18),
                    label: const Text('Share'),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// "Preview not available — open the file instead" state for the doc
/// preview sheet (server without preview-doc, or an empty preview).
class _DocPreviewUnavailable extends StatelessWidget {
  const _DocPreviewUnavailable({this.message, required this.onOpenFile});

  final String? message;
  final VoidCallback onOpenFile;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(
            Icons.insert_drive_file_outlined,
            size: 40,
            color: scheme.outline,
          ),
          const SizedBox(height: 8),
          Text(
            message ??
                'Preview not available — open the file instead.',
            style: Theme.of(context).textTheme.bodyMedium,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 12),
          FilledButton.icon(
            onPressed: onOpenFile,
            icon: const Icon(Icons.share_outlined),
            label: const Text('Download / share'),
          ),
        ],
      ),
    );
  }
}

/// Slide titles and text of a PPTX preview, one card per slide.
class _DocPreviewSlides extends StatelessWidget {
  const _DocPreviewSlides({required this.slides});

  final List<LibraryDocSlide> slides;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListView.separated(
      shrinkWrap: true,
      padding: const EdgeInsets.only(bottom: 8),
      itemCount: slides.length,
      separatorBuilder: (_, _) => const SizedBox(height: 12),
      itemBuilder: (context, i) {
        final slide = slides[i];
        return Card(
          elevation: 0,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(14),
            side: BorderSide(color: scheme.outlineVariant),
          ),
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Container(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 8,
                    vertical: 3,
                  ),
                  decoration: BoxDecoration(
                    color: scheme.primaryContainer,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Text(
                    'Slide ${slide.index}',
                    style: Theme.of(context).textTheme.labelSmall?.copyWith(
                      color: scheme.onPrimaryContainer,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                ),
                const SizedBox(height: 10),
                Text(
                  slide.title,
                  style: Theme.of(
                    context,
                  ).textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w700),
                ),
                if (slide.text.isNotEmpty) ...[
                  const SizedBox(height: 8),
                  Text(slide.text, style: Theme.of(context).textTheme.bodyMedium),
                ],
              ],
            ),
          ),
        );
      },
    );
  }
}

/// Sheet names and first rows of an XLSX preview, as a plain table.
class _DocPreviewSheets extends StatefulWidget {
  const _DocPreviewSheets({required this.sheets});

  final List<LibraryDocSheet> sheets;

  @override
  State<_DocPreviewSheets> createState() => _DocPreviewSheetsState();
}

class _DocPreviewSheetsState extends State<_DocPreviewSheets> {
  static const _maxShownRows = 50;
  int _selected = 0;

  @override
  Widget build(BuildContext context) {
    final idx = _selected.clamp(0, widget.sheets.length - 1);
    final sheet = widget.sheets[idx];
    final scheme = Theme.of(context).colorScheme;
    final header = sheet.rows.isNotEmpty ? sheet.rows.first : const <String>[];
    final dataRows = sheet.rows.length > 1
        ? sheet.rows.sublist(1)
        : const <List<String>>[];
    final shown = dataRows.take(_maxShownRows).toList();
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (widget.sheets.length > 1)
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Row(
              children: [
                for (var i = 0; i < widget.sheets.length; i++) ...[
                  ChoiceChip(
                    selected: i == idx,
                    label: Text(widget.sheets[i].name),
                    onSelected: (_) => setState(() => _selected = i),
                  ),
                  const SizedBox(width: 8),
                ],
              ],
            ),
          ),
        if (header.isEmpty)
          const Padding(
            padding: EdgeInsets.all(16),
            child: Text('This sheet has no cells to show.'),
          )
        else
          Flexible(
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(12),
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: DataTable(
                  headingRowColor: WidgetStatePropertyAll(
                    scheme.surfaceContainerHighest,
                  ),
                  border: TableBorder.all(
                    color: scheme.outlineVariant.withValues(alpha: 0.5),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  columns: [
                    for (final cell in header)
                      DataColumn(
                        label: Text(
                          cell,
                          style: const TextStyle(fontWeight: FontWeight.w700),
                        ),
                      ),
                  ],
                  rows: [
                    for (final row in shown)
                      DataRow(
                        cells: [
                          for (var c = 0; c < header.length; c++)
                            DataCell(Text(c < row.length ? row[c] : '')),
                        ],
                      ),
                  ],
                ),
              ),
            ),
          ),
        if (dataRows.length > shown.length)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              'Showing the first $_maxShownRows of ${dataRows.length} rows.',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
      ],
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
