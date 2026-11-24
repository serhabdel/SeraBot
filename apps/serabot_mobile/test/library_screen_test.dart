import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/bot_library_models.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/device.dart';
import 'package:serabot_mobile/screens/chat_screen.dart';
import 'package:serabot_mobile/screens/library_screen.dart';

const _token = '0123456789abcdef0123456789abcdef';
const _pngB64 =
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC';

const _imageHash =
    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const _deckHash =
    'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const _uploadHash =
    'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc';

http.Response _json(Object body, [int status = 200]) => http.Response(
  jsonEncode(body),
  status,
  headers: {'content-type': 'application/json'},
);

class _FakeLibraryDevice implements DeviceActions {
  PickedFile? nextUpload;
  bool throwTooLarge = false;
  final shared = <String>[];

  @override
  Future<List<PickedFile>> pickFiles({bool imagesOnly = false}) async =>
      const [];

  @override
  Future<PickedFile?> pickAnyFile({int maxBytes = maxAttachmentBytes}) async {
    if (throwTooLarge) {
      throw const FormatException('file_too_large');
    }
    return nextUpload;
  }

  @override
  Future<bool> startRecording() async => false;

  @override
  Future<RecordedAudio?> stopRecording() async => null;

  @override
  Future<void> cancelRecording() async {}

  @override
  Future<void> shareFile(String filename, List<int> bytes) async {
    shared.add(filename);
  }
}

void main() {
  testWidgets('LibraryScreen filters tabs, searches, toggles grid/list, previews and shares', (
    tester,
  ) async {
    final device = _FakeLibraryDevice();
    final client = ApiClient.connect(
      'https://example.com',
      _token,
      getRetryDelays: const [],
      httpClient: MockClient((req) async {
        final path = req.url.path;
        if (path == '/v1/library' && req.method == 'GET') {
          return _json({
            'enabled': true,
            'onedrive': {
              'mirror': true,
              'folder': 'SeraBot Library',
              'connected': true,
            },
            'items': [
              {
                'kind': 'generated',
                'id': _imageHash,
                'tool': 'make_image',
                'name': 'chart.png',
                'filename': 'chart.png',
                'mime': 'image/png',
                'size_bytes': 1234,
                'conversation_id': 'dir:main',
                'conversation_title': 'Main chat',
                'created_at': '${todayUtcDay()}T09:00:00Z',
              },
              {
                'kind': 'generated',
                'id': _deckHash,
                'tool': 'deck_build',
                'name': 'deck.pptx',
                'filename': 'deck.pptx',
                'mime': 'application/vnd.openxmlformats',
                'size_bytes': 4096,
                'conversation_id': 'dir:main',
                'conversation_title': 'Main chat',
                'created_at': '${todayUtcDay()}T08:00:00Z',
              },
              {
                'kind': 'uploaded',
                'id': _uploadHash,
                'tool': 'library_upload',
                'name': 'receipt.txt',
                'filename': 'receipt.txt',
                'mime': 'text/plain',
                'size_bytes': 42,
                'conversation_id': 'library',
                'created_at': '${todayUtcDay()}T07:00:00Z',
              },
            ],
          });
        }
        if (path == '/v1/library/thumbnail') {
          return http.Response.bytes(base64Decode(_pngB64), 200);
        }
        if (path == '/v1/artifacts/$_imageHash') {
          return http.Response.bytes(base64Decode(_pngB64), 200);
        }
        if (path == '/v1/library/preview-doc') {
          return _json({
            'kind': 'pptx',
            'slides': [
              {'index': 1, 'title': 'Summary Slide', 'text': 'Key points'},
            ],
          });
        }
        return _json({'error': 'not found'}, 404);
      }),
    );
    final app = AppState(client);
    addTearDown(app.dispose);

    await tester.pumpWidget(
      MaterialApp(
        home: LibraryScreen(app: app, device: device),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('OneDrive: SeraBot Library'), findsOneWidget);
    expect(find.text('chart.png'), findsOneWidget);
    expect(find.text('deck.pptx'), findsOneWidget);
    expect(find.text('receipt.txt'), findsOneWidget);

    // Filter to Uploaded tab.
    await tester.tap(find.byKey(const Key('global-library-tab-uploaded')));
    await tester.pumpAndSettle();
    expect(find.text('receipt.txt'), findsOneWidget);
    expect(find.text('chart.png'), findsNothing);

    // Filter to Made by bots tab.
    await tester.tap(find.byKey(const Key('global-library-tab-madeByBots')));
    await tester.pumpAndSettle();
    expect(find.text('chart.png'), findsOneWidget);
    expect(find.text('deck.pptx'), findsOneWidget);
    expect(find.text('receipt.txt'), findsNothing);

    // Search narrows items.
    await tester.enterText(
      find.byKey(const Key('global-library-search')),
      'deck',
    );
    await tester.pumpAndSettle();
    expect(find.text('deck.pptx'), findsOneWidget);
    expect(find.text('chart.png'), findsNothing);

    // Tap PPTX opens preview-doc sheet.
    await tester.tap(find.byKey(ValueKey('global-library-$_deckHash')));
    await tester.pumpAndSettle();
    expect(find.text('Summary Slide'), findsOneWidget);
    expect(find.text('Key points'), findsOneWidget);
    Navigator.of(tester.element(find.text('Summary Slide'))).pop();
    await tester.pumpAndSettle();

    // Clear search and toggle Grid view.
    await tester.tap(find.byTooltip('Clear search'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('global-library-view-toggle')));
    await tester.pumpAndSettle();
    expect(find.byKey(ValueKey('global-library-grid-$_imageHash')), findsOneWidget);

    // Tap image opens InteractiveViewer and share records the filename.
    await tester.tap(find.byKey(ValueKey('global-library-grid-$_imageHash')));
    await tester.pumpAndSettle();
    expect(find.byType(InteractiveViewer), findsOneWidget);
    await tester.tap(find.text('Share'));
    await tester.pumpAndSettle();
    expect(device.shared, contains('chart.png'));
  });

  testWidgets('LibraryScreen upload button picks file, uploads, and shows OneDrive status', (
    tester,
  ) async {
    final device = _FakeLibraryDevice()
      ..nextUpload = const PickedFile('upload-note.md', [35, 32, 72, 105]);
    var uploaded = false;
    final client = ApiClient.connect(
      'https://example.com',
      _token,
      getRetryDelays: const [],
      httpClient: MockClient((req) async {
        if (req.url.path == '/v1/library' && req.method == 'GET') {
          return _json({
            'enabled': true,
            'onedrive': {
              'mirror': true,
              'folder': 'SeraBot Library',
              'connected': true,
            },
            'items': [
              if (uploaded)
                {
                  'kind': 'uploaded',
                  'id': _uploadHash,
                  'tool': 'library_upload',
                  'name': 'upload-note.md',
                  'filename': 'upload-note.md',
                  'mime': 'text/markdown',
                  'size_bytes': 4,
                  'conversation_id': 'library',
                  'created_at': '${todayUtcDay()}T10:00:00Z',
                },
            ],
          });
        }
        if (req.url.path == '/v1/library/upload' && req.method == 'POST') {
          expect(req.headers['X-Filename'], 'upload-note.md');
          expect(req.bodyBytes, [35, 32, 72, 105]);
          uploaded = true;
          return _json({
            'item': {
              'kind': 'uploaded',
              'id': _uploadHash,
              'tool': 'library_upload',
              'name': 'upload-note.md',
              'filename': 'upload-note.md',
              'mime': 'text/markdown',
              'size_bytes': 4,
              'conversation_id': 'library',
              'created_at': '${todayUtcDay()}T10:00:00Z',
            },
            'onedrive': 'uploaded',
          });
        }
        return _json({}, 404);
      }),
    );
    final app = AppState(client);
    addTearDown(app.dispose);

    await tester.pumpWidget(
      MaterialApp(
        home: LibraryScreen(app: app, device: device),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('global-library-upload')));
    await tester.pumpAndSettle();

    expect(
      find.text('Saved to Library and mirrored to OneDrive.'),
      findsWidgets,
    );
    expect(find.text('upload-note.md'), findsOneWidget);

    // Oversize file picked shows a validation snackbar instead of uploading.
    device.throwTooLarge = true;
    await tester.tap(find.byKey(const Key('global-library-upload')));
    await tester.pumpAndSettle();
    expect(find.text('Files must be 25 MB or smaller.'), findsOneWidget);
  });

  testWidgets('ChatScreen shows turn.progress heartbeat and needs_review banner with dismiss', (
    tester,
  ) async {
    String taskStatus = 'running';
    final client = ApiClient.connect(
      'https://example.com',
      _token,
      getRetryDelays: const [],
      httpClient: MockClient((req) async {
        final path = req.url.path;
        if (path == '/v1/conversations/dir:main/messages') {
          return _json({
            'messages': [
              {
                'id': 'm1',
                'role': 'user',
                'content': 'Send email',
                'created_at': '2026-11-24T09:00:00Z',
              },
            ],
          });
        }
        if (path == '/v1/messages' && req.method == 'POST') {
          return _json({
            'task': {
              'id': 't-heartbeat',
              'kind': 'turn',
              'bot': 'main',
              'status': 'queued',
            },
          });
        }
        if (path == '/v1/tasks/t-heartbeat') {
          return _json({
            'task': {
              'id': 't-heartbeat',
              'kind': 'turn',
              'bot': 'main',
              'status': taskStatus,
            },
          });
        }
        if (path.endsWith('/library')) {
          return _json({'artifacts': []});
        }
        if (path.endsWith('/activity') || path.endsWith('/plan')) {
          return _json({}, 404);
        }
        return _json({}, 404);
      }),
    );
    final app = AppState(client, eventDebounce: Duration.zero);
    addTearDown(app.dispose);
    final device = _FakeLibraryDevice();

    await tester.pumpWidget(
      MaterialApp(
        home: ChatScreen(
          app: app,
          conversation: const ConversationSummary(
            id: 'dir:main',
            kind: 'direct',
            title: 'Main',
          ),
          device: device,
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('composer-field')), 'Check in');
    await tester.pump();
    await tester.tap(find.byKey(const Key('send-button')));
    await tester.pump(const Duration(milliseconds: 50));

    // Before the first heartbeat, the default working text shows.
    expect(find.text('Working…'), findsWidgets);

    // turn.progress event updates the working indicator with elapsed seconds.
    app.handleEvent(
      const ServerEvent(10, 'turn.progress', {
        'task_id': 't-heartbeat',
        'conversation_id': 'dir:main',
        'elapsed_secs': 45,
        'beat': 3,
      }),
    );
    await tester.pump();
    expect(find.text('Thinking · 45s'), findsWidgets);

    // task.failed while task status is needs_review shows the amber warning banner.
    taskStatus = 'needs_review';
    app.handleEvent(
      const ServerEvent(11, 'task.failed', {
        'task_id': 't-heartbeat',
        'conversation_id': 'dir:main',
      }),
    );
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('needs-review-banner')), findsOneWidget);
    expect(
      find.text(
        'Something may have happened — check the result before trying again.',
      ),
      findsOneWidget,
    );
    expect(find.byKey(const Key('needs-review-dismiss')), findsOneWidget);
    // No spinner on the activity panel while parked in needs_review.
    expect(
      find.descendant(
        of: find.byKey(const Key('activity-panel')),
        matching: find.byType(CircularProgressIndicator),
      ),
      findsNothing,
    );

    // Tapping Dismiss / Reviewed clears the banner.
    await tester.tap(find.byKey(const Key('needs-review-dismiss')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('needs-review-banner')), findsNothing);

    await tester.pumpWidget(const SizedBox());
    await tester.pumpAndSettle();
  });
}
