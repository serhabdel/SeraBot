import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/preview_pane.dart';
import 'package:serabot_mobile/screens/chats_screen.dart';
import 'package:serabot_mobile/widgets.dart';

// Minimal valid 1x1 PNG bytes.
final _tinyPng = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
);

void main() {
  group('preview_pane parsers', () {
    test('parseSpreadsheetSheets parses multi-sheet TSV and numeric cells', () {
      const raw = '''
=== Sheet: Revenue ===
Month\tAmount
Jan\t1200
Feb\t1450.50

=== Sheet: Costs ===
Item,Cost
Hosting,99
''';
      final sheets = parseSpreadsheetSheets(raw, filename: 'q1.xlsx');
      expect(sheets, hasLength(2));
      expect(sheets[0].name, 'Revenue');
      expect(sheets[0].headers, ['Month', 'Amount']);
      expect(sheets[0].rows, [
        ['Jan', '1200'],
        ['Feb', '1450.50'],
      ]);
      expect(isNumericCell('1,450.50'), isTrue);
      expect(isNumericCell('Jan'), isFalse);
      expect(sheets[1].name, 'Costs');
      expect(sheets[1].rows.first, ['Hosting', '99']);
    });

    test('parsePresentationSlides parses slide markers', () {
      const raw = '''
=== Slide 1 ===
Executive Summary
- Revenue grew 22%
- Churn dropped

=== Slide 2 ===
Next Steps
- Launch mobile v0.47
''';
      final slides = parsePresentationSlides(raw);
      expect(slides, hasLength(2));
      expect(slides[0].number, 1);
      expect(slides[0].title, 'Executive Summary');
      expect(slides[0].lines, ['- Revenue grew 22%', '- Churn dropped']);
      expect(slides[1].number, 2);
      expect(slides[1].title, 'Next Steps');
    });

    test('parseDocumentBlocks parses headings and bullets', () {
      const raw = '''
# Project Charter
## Goals
- Ship cleanly
Body paragraph here.
''';
      final blocks = parseDocumentBlocks(raw);
      expect(blocks, hasLength(4));
      expect(blocks[0].headingLevel, 1);
      expect(blocks[1].headingLevel, 2);
      expect(blocks[2].isBullet, isTrue);
      expect(blocks[3].headingLevel, 0);
    });
  });

  group('smartConversationTitle', () {
    test('replaces raw generated ids with clean preview fallback', () {
      const rawSlug = ConversationSummary(
        id: 'dir:chat-abc123',
        kind: 'direct',
        title: 'dir:chat-abc123',
        lastMessage: LastMessage(role: 'user', text: 'Summarize Q4 earnings'),
      );
      expect(smartConversationTitle(rawSlug), 'Summarize Q4 earnings');

      const hexChat = ConversationSummary(
        id: 'dir:01a1dfd96',
        kind: 'direct',
        title: 'Chat 01a1dfd96',
      );
      expect(smartConversationTitle(hexChat), 'New conversation');

      const named = ConversationSummary(
        id: 'dir:main',
        kind: 'direct',
        title: 'Daily plan',
      );
      expect(smartConversationTitle(named), 'Daily plan');
    });
  });

  group('AppState avatar cache & SeraAvatar', () {
    testWidgets('caches bot avatar bytes even when version is null', (tester) async {
      var hits = 0;
      final httpClient = MockClient((req) async {
        if (req.url.path == '/v1/bots/jobfinder/avatar') {
          hits++;
          return http.Response.bytes(_tinyPng, 200);
        }
        return http.Response('', 404);
      });
      final api = ApiClient.connect('https://example.com', 'z' * 43,
          httpClient: httpClient);
      final app = AppState(api);
      addTearDown(app.dispose);

      expect(app.cachedAvatar('bots', 'jobfinder', null), isNull);
      final bytes = await app.avatar('bots', 'jobfinder', null);
      expect(bytes, isNotNull);
      expect(hits, 1);

      final cached = app.cachedAvatar('bots', 'jobfinder', null);
      expect(cached, isA<Uint8List>());
      expect(cached!.length, _tinyPng.length);

      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SeraAvatar(
              label: 'JobFinder',
              app: app,
              kind: 'bots',
              id: 'jobfinder',
            ),
          ),
        ),
      );
      // Frame 0 already has MemoryImage without needing pumpAndSettle.
      final avatar = tester.widget<CircleAvatar>(find.byType(CircleAvatar));
      expect(avatar.backgroundImage, isA<MemoryImage>());
    });

    testWidgets('renders per-bot gradient+glyph fallback when no image cached',
        (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: SeraAvatar(
              label: 'Analyst',
              kind: 'bots',
              id: 'analyst',
            ),
          ),
        ),
      );
      expect(find.byIcon(Icons.analytics_outlined), findsOneWidget);
    });
  });

  group('ArtifactPreviewPane visual viewers', () {
    testWidgets('renders spreadsheet DataTable and sheet tabs', (tester) async {
      final httpClient = MockClient((req) async {
        return http.Response(
          jsonEncode({
            'content_type': 'text/plain',
            'text': '=== Sheet: Sales ===\nRegion\tTotal\nEU\t500\n=== Sheet: Costs ===\nItem\tAmount\nAds\t100',
          }),
          200,
        );
      });
      final api = ApiClient.connect('https://example.com', 'z' * 43,
          httpClient: httpClient);
      final entry = LibraryEntry(
        actionId: 'a1',
        tool: 'sheets',
        hash: 'a' * 64,
        filename: 'report.xlsx',
      );
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: ArtifactPreviewPane(client: api, entry: entry),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('Sales'), findsOneWidget);
      expect(find.text('Costs'), findsOneWidget);
      expect(find.byType(DataTable), findsOneWidget);
      expect(find.text('EU'), findsOneWidget);
    });
  });
}
