import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/bot_library_models.dart';
import 'package:serabot_mobile/screens/bot_library_screen.dart';

const _token = '0123456789abcdef0123456789abcdef';
const _pngB64 =
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC';

const _imageHash = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const _deckHash = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';

http.Response _json(Object body) => http.Response(
  jsonEncode(body),
  200,
  headers: {'content-type': 'application/json'},
);

Map<String, dynamic> _row({
  required String id,
  required String filename,
  required String mime,
  int? size,
}) => {
  'kind': 'generated',
  'id': id,
  'tool': 'make_file',
  'name': filename,
  'filename': filename,
  'mime': mime,
  'size_bytes': size,
  'conversation_id': 'dir:synthetic',
  'conversation_title': 'Synthetic chat',
  'created_at': '${todayUtcDay()}T08:00:00Z',
};

/// Serves the bot library plus artifact preview/download. The doc-preview
/// route always 404s, like a 0.74.8 server without preview-doc.
Future<http.Response> _handler(http.Request request) async {
  final path = request.url.path;
  if (path == '/v1/bots/synthetic-bot/library') {
    return _json({
      'bot': 'synthetic-bot',
      'items': [
        _row(
          id: _imageHash,
          filename: 'photo.png',
          mime: 'image/png',
          size: 1234,
        ),
        _row(id: _deckHash, filename: 'deck.pptx', mime: 'application/vnd.ms'),
      ],
    });
  }
  if (path == '/v1/artifacts/$_imageHash/preview') {
    return _json({'content_type': 'image/png', 'image_b64': _pngB64});
  }
  if (path == '/v1/artifacts/$_imageHash') {
    return http.Response.bytes(base64Decode(_pngB64), 200);
  }
  return http.Response('not found', 404);
}

AppState _app() {
  final client = ApiClient.connect(
    'https://example.com',
    _token,
    getRetryDelays: const [],
    httpClient: MockClient(_handler),
  );
  return AppState(client);
}

void main() {
  testWidgets('0.74.9 library tiles show thumbnails, size, date, badges', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: BotLibraryScreen(
          app: _app(),
          botId: 'synthetic-bot',
          botName: 'Synthetic bot',
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('photo.png'), findsOneWidget);
    expect(find.text('1 KB · ${todayUtcDay()}'), findsOneWidget);
    expect(find.text('PNG'), findsOneWidget);
    expect(find.text('PPTX'), findsOneWidget);
    // The image thumbnail resolved from the artifact preview.
    expect(find.byType(Image), findsWidgets);
  });

  testWidgets('0.74.9 tapping an image opens the viewer with prev/next', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: BotLibraryScreen(
          app: _app(),
          botId: 'synthetic-bot',
          botName: 'Synthetic bot',
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(ValueKey('library-$_imageHash')));
    await tester.pumpAndSettle();

    expect(find.text('1 of 1'), findsOneWidget);
    expect(find.text('Share'), findsOneWidget);
    expect(find.byType(InteractiveViewer), findsOneWidget);

    await tester.tap(find.byTooltip('Close'));
    await tester.pumpAndSettle();
    expect(find.text('photo.png'), findsOneWidget);
  });

  testWidgets('0.74.9 doc preview without the server route offers the file', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: BotLibraryScreen(
          app: _app(),
          botId: 'synthetic-bot',
          botName: 'Synthetic bot',
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(ValueKey('library-$_deckHash')));
    await tester.pumpAndSettle();

    expect(
      find.text('Preview not available — open the file instead.'),
      findsOneWidget,
    );
    expect(find.text('Download / share'), findsOneWidget);
  });
}
