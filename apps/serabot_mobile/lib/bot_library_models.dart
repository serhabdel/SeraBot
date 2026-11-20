// Bot library (0.63): parsing and view rules for GET /v1/bots/{bot}/library.
// Mirrors crates/serabot-desktop/src/bot_library.rs: the same kind filters and
// search, grouped by date. Counts, names and dates only.

import 'usage_insights_models.dart';
import 'validation.dart';

/// Most items the phone keeps from one library response.
const maxLibraryItems = 3500;

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
