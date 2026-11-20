import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/bot_library_models.dart';

Map<String, dynamic> _row({
  String kind = 'generated',
  String id = 'artifact_one',
  String? name,
  String? filename,
  String mime = 'application/pdf',
  Object? size,
  String createdAt = '2026-11-20T08:00:00Z',
  String? title = 'Synthetic chat',
  String tool = 'make_report',
}) {
  return {
    'kind': kind,
    'id': id,
    'tool': tool,
    'name': name,
    'filename': filename,
    'mime': mime,
    'size_bytes': size,
    'conversation_id': 'dir:synthetic',
    'conversation_title': title,
    'created_at': createdAt,
  };
}

void main() {
  test('parses a library response and skips unusable rows', () {
    final items = parseBotLibrary({
      'bot': 'synthetic-bot',
      'items': [
        _row(name: 'Quarterly summary', filename: 'summary.pdf', size: 2048),
        _row(
          kind: 'uploaded',
          id: 'att_upload_1',
          tool: 'upload',
          filename: 'photo.png',
          mime: 'image/png',
          createdAt: '2026-11-18T12:00:00Z',
          title: null,
        ),
        _row(kind: 'mystery', id: 'bad_kind'),
        _row(id: 'bad id with spaces'),
        _row(createdAt: 'nope'),
        'not an object',
      ],
    });

    expect(items.map((i) => i.id), ['artifact_one', 'att_upload_1']);
    final made = items.first;
    expect(made.generated, isTrue);
    expect(made.label, 'Quarterly summary');
    expect(made.sizeBytes, 2048);
    expect(made.extension, 'pdf');
    expect(made.kindLabel, 'Made by a bot');
    expect(made.origin, 'Made by make_report in Synthetic chat');
    expect(made.day, '2026-11-20');

    final uploaded = items.last;
    expect(uploaded.generated, isFalse);
    expect(uploaded.isMedia, isTrue);
    expect(uploaded.sizeBytes, isNull);
    expect(uploaded.label, 'photo.png');
    expect(uploaded.origin, 'Uploaded in a chat');
  });

  test('a missing bundle of items reads as an empty library', () {
    expect(parseBotLibrary({'bot': 'synthetic-bot'}), isEmpty);
    expect(parseBotLibrary({'items': 'nope'}), isEmpty);
  });

  test('label falls back from title, to file name, to a short id', () {
    final byId = BotLibraryItem.fromJson(
      _row(id: 'abcdefghijklmnopqrstuvwxyz', name: null, filename: null),
    );
    expect(byId!.label, 'abcdefghijkl');
    final byName = BotLibraryItem.fromJson(
      _row(name: '  ', filename: 'notes.md'),
    );
    expect(byName!.label, 'notes.md');
  });

  test('kind filters admit the right items', () {
    final made = BotLibraryItem.fromJson(
      _row(filename: 'deck.pptx', mime: 'application/vnd.openxmlformats'),
    )!;
    final upload = BotLibraryItem.fromJson(
      _row(
        kind: 'uploaded',
        id: 'att_2',
        filename: 'clip.mp4',
        mime: 'video/mp4',
      ),
    )!;
    expect(LibraryKind.all.admits(made), isTrue);
    expect(LibraryKind.madeByBots.admits(made), isTrue);
    expect(LibraryKind.madeByBots.admits(upload), isFalse);
    expect(LibraryKind.uploaded.admits(upload), isTrue);
    expect(LibraryKind.media.admits(upload), isTrue);
    expect(LibraryKind.media.admits(made), isFalse);
    expect(LibraryKind.documents.admits(made), isTrue);
    expect(LibraryKind.documents.admits(upload), isFalse);
  });

  test('view filters by kind and search, newest first', () {
    final items = parseBotLibrary({
      'items': [
        _row(
          id: 'old',
          name: 'Budget draft',
          createdAt: '2026-10-01T09:00:00Z',
        ),
        _row(
          id: 'new',
          name: 'Budget final',
          createdAt: '2026-11-20T09:00:00Z',
        ),
        _row(
          kind: 'uploaded',
          id: 'up',
          filename: 'Budget.xlsx',
          mime: 'application/vnd',
          createdAt: '2026-11-15T09:00:00Z',
        ),
      ],
    });

    final all = libraryView(items);
    expect(all.map((i) => i.id), ['new', 'up', 'old']);

    final made = libraryView(items, kind: LibraryKind.madeByBots);
    expect(made.map((i) => i.id), ['new', 'old']);

    final searched = libraryView(items, query: '  BUDGET final ');
    expect(searched.map((i) => i.id), ['new']);

    final byChat = libraryView(items, query: 'synthetic chat');
    expect(byChat, hasLength(3));

    expect(libraryView(items, query: 'nothing matches'), isEmpty);
  });

  test('date groups: Today, This week, Earlier', () {
    const today = '2026-11-20';
    expect(libraryDateGroup('2026-11-20', today), 'Today');
    expect(libraryDateGroup('2026-11-19', today), 'This week');
    expect(libraryDateGroup('2026-11-14', today), 'This week');
    expect(libraryDateGroup('2026-11-13', today), 'Earlier');
    expect(libraryDateGroup('2026-03-01', today), 'Earlier');
    expect(libraryDateGroup('garbage', today), 'Earlier');
    expect(libraryDateGroup('2026-11-20', 'garbage'), 'Earlier');
  });

  test('groups keep the order and drop empty groups', () {
    final items = parseBotLibrary({
      'items': [
        _row(id: 'a', createdAt: '2026-01-02T09:00:00Z'),
        _row(id: 'b', createdAt: '2026-11-20T09:00:00Z'),
      ],
    });
    final groups = groupLibraryItems(libraryView(items), '2026-11-20');
    expect(groups.map((g) => g.heading), ['Today', 'Earlier']);
    expect(groups.first.items.single.id, 'b');
    expect(groups.last.items.single.id, 'a');
  });

  test('today is a UTC calendar day', () {
    expect(RegExp(r'^\d{4}-\d{2}-\d{2}$').hasMatch(todayUtcDay()), isTrue);
  });
}
