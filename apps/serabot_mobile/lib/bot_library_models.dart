// Bot library (0.63): parsing and view rules for GET /v1/bots/{bot}/library.
// Mirrors crates/serabot-desktop/src/bot_library.rs: the same kind filters and
// search, grouped by date. Counts, names and dates only.
//
// 0.74.9: type helpers for thumbnails/viewer/doc preview (isImage, isPdf,
// docPreviewKind, isArtifactHash) and the parsed
// `GET /v1/library/preview-doc` body ([LibraryDocPreview]).

import 'usage_insights_models.dart';
import 'validation.dart';

/// Most items the phone keeps from one library response.
const maxLibraryItems = 3500;

/// Image extensions Flutter can render from bytes (no SVG: it needs a
/// vector renderer the app does not ship).
const libraryImageExtensions = {'png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'};

/// Which items the library shows.
enum LibraryKind {
  all('All'),
  madeByBots('Made by bots'),
  uploaded('Uploaded'),
  media('Media'),
  documents('Documents');

  const LibraryKind(this.label);
  final String label;

  bool admits(BotLibraryItem item) {
    return switch (this) {
      LibraryKind.all => true,
      LibraryKind.madeByBots => item.generated,
      LibraryKind.uploaded => !item.generated,
      LibraryKind.media => item.isMedia,
      LibraryKind.documents => !item.isMedia,
    };
  }
}

/// One file in a bot's library: made by a bot, or uploaded by the member.
class BotLibraryItem {
  const BotLibraryItem({
    required this.generated,
    required this.id,
    required this.label,
    required this.conversationId,
    required this.createdAt,
    this.tool = '',
    this.filename,
    this.mime = '',
    this.sizeBytes,
    this.conversationTitle,
  });

  /// Null when the row is unusable (no id, unknown kind or no date).
  static BotLibraryItem? fromJson(Map<String, dynamic> json) {
    final kind = insightText(json['kind'], 16);
    if (kind != 'generated' && kind != 'uploaded') {
      return null;
    }
    final id = insightText(json['id'], 128);
    final createdAt = insightText(json['created_at'], 64);
    final conversationId = insightText(json['conversation_id'], 128);
    if (!validId(id) || createdAt.length < 10) {
      return null;
    }
    final filename = insightOptText(json['filename'], 255);
    final name = insightOptText(json['name'], 255);
    final size = json['size_bytes'];
    return BotLibraryItem(
      generated: kind == 'generated',
      id: id,
      label:
          name ?? filename ?? id.substring(0, id.length < 12 ? id.length : 12),
      tool: insightText(json['tool'], 64),
      filename: filename,
      mime: insightText(json['mime'], 128),
      sizeBytes: size is num && size.isFinite && size >= 0
          ? size.toInt()
          : null,
      conversationId: conversationId,
      conversationTitle: insightOptText(json['conversation_title'], 200),
      createdAt: createdAt,
    );
  }

  final bool generated;
  final String id;
  final String label;
  final String tool;
  final String? filename;
  final String mime;
  final int? sizeBytes;
  final String conversationId;
  final String? conversationTitle;

  /// ISO 8601 time as the server sent it (UTC).
  final String createdAt;

  bool get isMedia =>
      mime.startsWith('image/') ||
      mime.startsWith('audio/') ||
      mime.startsWith('video/');

  /// True for images Flutter can render from bytes. SVG is excluded: it
  /// needs a vector renderer the app does not ship.
  bool get isImage {
    if (libraryImageExtensions.contains(extension)) {
      return true;
    }
    return mime.startsWith('image/') && extension != 'svg';
  }

  bool get isPdf => extension == 'pdf';

  /// `'pptx'` or `'xlsx'` when the file is one the doc-preview endpoint
  /// covers, otherwise null.
  String? get docPreviewKind {
    final ext = extension;
    return ext == 'pptx' || ext == 'xlsx' ? ext : null;
  }

  /// True when [id] is a 64-hex artifact hash (generated items). Uploaded
  /// items carry an attachment id instead, which the artifact endpoints
  /// (preview, preview-doc, download by hash) do not accept.
  bool get isArtifactHash => RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(id);

  /// File name for a download/share. Never empty.
  String get downloadName {
    final name = filename?.trim();
    if (name != null && name.isNotEmpty) {
      return name;
    }
    final shown = label.trim();
    return shown.isNotEmpty ? shown : 'file';
  }

  /// "pptx", "pdf" and so on, for the type chip. Empty when unknown.
  String get extension {
    final name = filename;
    if (name == null) {
      return '';
    }
    final dot = name.lastIndexOf('.');
    if (dot < 0 || dot == name.length - 1) {
      return '';
    }
    final ext = name.substring(dot + 1).toLowerCase();
    return ext.length <= 5 ? ext : '';
  }

  /// `YYYY-MM-DD` of the item (UTC).
  String get day => createdAt.substring(0, 10);

  String get kindLabel => generated ? 'Made by a bot' : 'Uploaded';

  String get origin {
    final where = conversationTitle ?? 'a chat';
    if (generated) {
      return tool.isEmpty
          ? 'Made by a bot in $where'
          : 'Made by $tool in $where';
    }
    return 'Uploaded in $where';
  }

  String get searchText =>
      '$label ${filename ?? ''} $tool ${conversationTitle ?? ''}'.toLowerCase();
}

/// Parses the `items` list of a library response. Bad rows are skipped and the
/// list is bounded, so one odd row never hides the rest.
List<BotLibraryItem> parseBotLibrary(Map<String, dynamic> json) {
  final rows = json['items'];
  if (rows is! List) {
    return const [];
  }
  final items = <BotLibraryItem>[];
  for (final row in rows) {
    if (items.length >= maxLibraryItems) {
      break;
    }
    if (row is Map<String, dynamic>) {
      final item = BotLibraryItem.fromJson(row);
      if (item != null) {
        items.add(item);
      }
    }
  }
  return items;
}

/// The items to show: filtered by kind and search, newest first.
List<BotLibraryItem> libraryView(
  List<BotLibraryItem> items, {
  LibraryKind kind = LibraryKind.all,
  String query = '',
}) {
  final needle = query.trim().toLowerCase();
  final out = items
      .where((item) => kind.admits(item))
      .where((item) => needle.isEmpty || item.searchText.contains(needle))
      .toList();
  out.sort((a, b) => b.createdAt.compareTo(a.createdAt));
  return out;
}

/// Today, This week (the six days before today) or Earlier. Days are compared
/// in UTC, as the server stores them. An unreadable day counts as Earlier.
String libraryDateGroup(String day, String today) {
  final item = _utcDay(day);
  final now = _utcDay(today);
  if (item == null || now == null) {
    return 'Earlier';
  }
  final daysAgo = now.difference(item).inDays;
  if (daysAgo <= 0) {
    return 'Today';
  }
  return daysAgo <= 6 ? 'This week' : 'Earlier';
}

/// Today's date as `YYYY-MM-DD`, in UTC.
String todayUtcDay() {
  final now = DateTime.now().toUtc();
  return '${now.year.toString().padLeft(4, '0')}-'
      '${now.month.toString().padLeft(2, '0')}-'
      '${now.day.toString().padLeft(2, '0')}';
}

/// The items in group order: Today, This week, Earlier. Empty groups are left
/// out.
List<({String heading, List<BotLibraryItem> items})> groupLibraryItems(
  List<BotLibraryItem> items,
  String today,
) {
  const order = ['Today', 'This week', 'Earlier'];
  final buckets = <String, List<BotLibraryItem>>{
    for (final heading in order) heading: <BotLibraryItem>[],
  };
  for (final item in items) {
    buckets[libraryDateGroup(item.day, today)]!.add(item);
  }
  return [
    for (final heading in order)
      if (buckets[heading]!.isNotEmpty)
        (heading: heading, items: buckets[heading]!),
  ];
}

DateTime? _utcDay(String day) {
  final match = RegExp(r'^(\d{4})-(\d{2})-(\d{2})').firstMatch(day);
  if (match == null) {
    return null;
  }
  final year = int.parse(match[1]!);
  final month = int.parse(match[2]!);
  final date = int.parse(match[3]!);
  if (month < 1 || month > 12 || date < 1 || date > 31) {
    return null;
  }
  return DateTime.utc(year, month, date);
}

/// One slide of a PPTX doc preview (`GET /v1/library/preview-doc`, 0.74.9).
class LibraryDocSlide {
  const LibraryDocSlide({
    required this.index,
    required this.title,
    required this.text,
  });

  final int index;
  final String title;
  final String text;
}

/// One sheet of an XLSX doc preview. Rows are raw cell strings; the first
/// row is the header when the server sent one.
class LibraryDocSheet {
  const LibraryDocSheet({required this.name, required this.rows});

  final String name;
  final List<List<String>> rows;
}

/// Parsed `GET /v1/library/preview-doc?hash=...&kind=pptx|xlsx` body:
/// `{kind: 'pptx', slides: [{index, title, text}]}` or
/// `{kind: 'xlsx', sheets: [{name, rows: [[cell, ...]]}]}`.
///
/// Parsing is defensive and bounded: a wrong `kind` reads as null, bad rows
/// are skipped, and slides/rows/cells are capped, so one odd body never
/// crashes the preview sheet.
class LibraryDocPreview {
  const LibraryDocPreview({
    required this.kind,
    this.slides = const [],
    this.sheets = const [],
  });

  static const maxSlides = 200;
  static const maxSheets = 50;
  static const maxRows = 200;
  static const maxCells = 50;

  final String kind;
  final List<LibraryDocSlide> slides;
  final List<LibraryDocSheet> sheets;

  static LibraryDocPreview? fromJson(Map<String, dynamic> json) {
    final kind = insightText(json['kind'], 16);
    if (kind != 'pptx' && kind != 'xlsx') {
      return null;
    }
    final slides = <LibraryDocSlide>[];
    if (kind == 'pptx' && json['slides'] is List) {
      var position = 0;
      for (final row in (json['slides'] as List).take(maxSlides)) {
        position++;
        if (row is! Map<String, dynamic>) {
          continue;
        }
        final index = row['index'];
        final title = insightText(row['title'], 500);
        final text = insightText(row['text'], 4000);
        if (title.isEmpty && text.isEmpty) {
          continue;
        }
        slides.add(
          LibraryDocSlide(
            index: index is num && index.isFinite ? index.toInt() : position,
            title: title,
            text: text,
          ),
        );
      }
    }
    final sheets = <LibraryDocSheet>[];
    if (kind == 'xlsx' && json['sheets'] is List) {
      var position = 0;
      for (final row in (json['sheets'] as List).take(maxSheets)) {
        position++;
        if (row is! Map<String, dynamic>) {
          continue;
        }
        final name =
            insightOptText(row['name'], 200) ?? 'Sheet $position';
        final rows = <List<String>>[];
        if (row['rows'] is List) {
          for (final cells in (row['rows'] as List).take(maxRows)) {
            if (cells is! List) {
              continue;
            }
            rows.add([
              for (final cell in cells.take(maxCells)) _docCell(cell),
            ]);
          }
        }
        if (rows.isEmpty) {
          continue;
        }
        sheets.add(LibraryDocSheet(name: name, rows: rows));
      }
    }
    return LibraryDocPreview(kind: kind, slides: slides, sheets: sheets);
  }

  static String _docCell(Object? cell) {
    if (cell is String) {
      return insightText(cell, 1000);
    }
    if (cell is num || cell is bool) {
      return cell.toString();
    }
    return '';
  }
}

/// OneDrive mirror status reported by `GET /v1/library` (0.74.8).
class LibraryOnedriveStatus {
  const LibraryOnedriveStatus({
    this.mirror = false,
    this.folder = '',
    this.connected = false,
  });

  factory LibraryOnedriveStatus.fromJson(Object? raw) {
    final json = raw is Map<String, dynamic> ? raw : const <String, dynamic>{};
    return LibraryOnedriveStatus(
      mirror: json['mirror'] as bool? ?? false,
      folder: insightText(json['folder'], 256),
      connected: json['connected'] as bool? ?? false,
    );
  }

  final bool mirror;
  final String folder;
  final bool connected;

  String get statusLabel {
    if (!mirror) {
      return 'OneDrive mirror off';
    }
    if (!connected) {
      return 'OneDrive: not connected';
    }
    return folder.isEmpty ? 'OneDrive: connected' : 'OneDrive: $folder';
  }
}

/// Parsed `GET /v1/library` payload: all items across chats + uploads, and
/// the per-account OneDrive mirror status.
class GlobalLibraryResponse {
  const GlobalLibraryResponse({
    this.enabled = true,
    this.onedrive = const LibraryOnedriveStatus(),
    this.items = const [],
  });

  factory GlobalLibraryResponse.fromJson(Map<String, dynamic> json) {
    return GlobalLibraryResponse(
      enabled: json['enabled'] as bool? ?? true,
      onedrive: LibraryOnedriveStatus.fromJson(json['onedrive']),
      items: parseBotLibrary(json),
    );
  }

  final bool enabled;
  final LibraryOnedriveStatus onedrive;
  final List<BotLibraryItem> items;
}

/// Parsed `POST /v1/library/upload` reply: the created item (if present) and
/// the best-effort OneDrive mirror outcome ("uploaded", "off",
/// "OneDrive is not connected...", or "failed: ...").
class LibraryUploadResponse {
  const LibraryUploadResponse({this.item, this.onedrive = ''});

  factory LibraryUploadResponse.fromJson(Map<String, dynamic> json) {
    final rawItem = json['item'];
    return LibraryUploadResponse(
      item: rawItem is Map<String, dynamic>
          ? BotLibraryItem.fromJson(rawItem)
          : null,
      onedrive: insightText(json['onedrive'], 256),
    );
  }

  final BotLibraryItem? item;
  final String onedrive;

  String get onedriveSummary {
    final s = onedrive.trim();
    if (s.isEmpty || s == 'off') {
      return 'Saved to Library (OneDrive mirror off).';
    }
    if (s == 'uploaded') {
      return 'Saved to Library and mirrored to OneDrive.';
    }
    return 'Saved to Library. OneDrive: $s';
  }
}

