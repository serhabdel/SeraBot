import 'dart:convert';

import 'package:material_ui/material_ui.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/auth.dart';
import 'package:serabot_mobile/device.dart';
import 'package:serabot_mobile/main.dart';
import 'package:serabot_mobile/session.dart';
import 'package:serabot_mobile/theme.dart';

class MemoryStore implements TokenStore {
  String? endpoint;
  String? token;

  @override
  Future<({String endpoint, String token})?> read() async =>
      token == null ? null : (endpoint: endpoint!, token: token!);

  @override
  Future<void> write(String endpoint, String token) async {
    this.endpoint = endpoint;
    this.token = token;
  }

  @override
  Future<void> clear() async => token = null;
}

class FakeDevice implements DeviceActions {
  List<PickedFile> files = [];
  bool micAllowed = true;
  bool recording = false;
  final shared = <String>[];

  @override
  Future<List<PickedFile>> pickFiles({bool imagesOnly = false}) async => files;
  @override
  Future<bool> startRecording() async => recording = micAllowed;
  @override
  Future<RecordedAudio?> stopRecording() async =>
      const RecordedAudio('voice-note.m4a', [1, 2, 3]);
  @override
  Future<void> cancelRecording() async {}
  @override
  Future<void> shareFile(String filename, List<int> bytes) async =>
      shared.add(filename);
}

OwnerSession _session(TokenStore store, http.Client client) => OwnerSession(
      store: store,
      authApi: AuthApi(httpClient: client),
      httpClient: client,
    );

http.Response _json(Object body, [int status = 200]) =>
    http.Response(jsonEncode(body), status,
        headers: {'content-type': 'application/json'});

/// A tiny SeraBot server: the same user-scoped conversations the desktop sees.
class FakeServer {
  final requests = <String>[];
  final bodies = <String, Object?>{};
  String? modelError;
  bool transcribeOk = true;

  Future<http.Response> handle(http.Request r) async {
    final key = '${r.method} ${r.url.path}';
    requests.add(key);
    if (r.bodyBytes.isNotEmpty && r.headers['Content-Type'] == 'application/json') {
      bodies[key] = jsonDecode(r.body);
    }
    switch (key) {
      case 'GET /v1/health':
        return _json({'ok': true, 'version': '0.21.0'});
      case 'GET /v1/me':
        return _json({'id': 'u1', 'email': 'ada@example.com', 'display_name': 'Ada', 'role': 'member'});
      case 'GET /v1/conversations':
        return _json({
          'conversations': [
            {'id': 'dir:main', 'kind': 'direct', 'title': 'Daily plan', 'created_at': 'a', 'updated_at': '2026-01-02T10:00:00Z', 'unread_count': 2, 'accepts_turns': true, 'last_message': {'role': 'assistant', 'text': 'Here is your plan'}},
            {'id': 'sec:fin', 'kind': 'section', 'title': 'Finance', 'created_at': 'a', 'updated_at': 'b', 'accepts_turns': false},
            {'id': 'dir:tax', 'kind': 'direct', 'parent_id': 'sec:fin', 'title': 'Tax 2026', 'created_at': 'a', 'updated_at': 'b', 'accepts_turns': true},
            {'id': 'room:team', 'kind': 'room', 'title': 'Review team', 'created_at': 'a', 'updated_at': 'b', 'accepts_turns': true},
          ]
        });
      case 'GET /v1/bots':
        return _json({'bots': [{'id': 'main', 'role': 'orchestrator', 'enabled': true, 'live': true, 'display_name': 'Sera', 'tools': [], 'skills': []}]});
      case 'GET /v1/approvals':
        return _json({'approvals': [{'id': 'ap1', 'action_id': 'act1', 'tool': 'send_email', 'summary': 'Send the report', 'expires_at': '2099-01-01T00:00:00Z'}]});
      case 'GET /v1/events':
        return http.Response('', 200);
      case 'GET /v1/conversations/dir:main/messages':
        return _json({'messages': [
          {'id': 'm1', 'role': 'user', 'content': 'Plan my day', 'created_at': '2026-01-02T09:00:00Z'},
          {'id': 'm2', 'role': 'assistant', 'content': 'Here is **your** plan', 'created_at': '2026-01-02T09:01:00Z', 'bot': 'main'},
        ]});
      case 'GET /v1/conversations/dir:main/library':
        return _json({'artifacts': []});
      case 'GET /v1/models':
        return _json({
          'providers': [{'id': 'prov', 'models': ['m1'], 'data_egress_approved': true}],
          'entries': [],
          'selection': {'provider': 'auto', 'model': ''},
        });
      case 'PUT /v1/conversations/dir:main/model':
        if (modelError != null) {
          return _json({'error': modelError}, 409);
        }
        return _json({'selection': {'provider': 'prov', 'model': 'm1'}});
      case 'POST /v1/conversations/dir:main/attachments':
        return _json({'attachment': {'id': 'att-1', 'filename': r.headers['X-Filename'], 'size_bytes': 3}});
      case 'POST /v1/messages':
        return _json({'task': {'id': 't1', 'kind': 'turn', 'bot': 'main', 'status': 'queued'}});
      case 'POST /v1/audio/transcribe':
        return transcribeOk ? _json({'text': 'hello from voice'}) : _json({'error': 'audio is not configured'}, 503);
      case 'GET /v1/connectors':
        return _json({'connectors': [{'id': 'gmail', 'kind': 'gmail', 'enabled': true, 'personal': {'state': 'ready'}}]});
      case 'GET /v1/usage':
        return _json({'totals': {'calls': 12, 'total_tokens': 34567}, 'per_model': []});
      case 'POST /v1/approvals/ap1/decide':
        return _json({});
      case 'POST /v1/groups':
        return _json({'group': {'id': 'room:sera', 'kind': 'room', 'title': 'Sera', 'created_at': 'a', 'updated_at': 'b', 'accepts_turns': true}});
      case 'GET /v1/conversations/room:sera/messages':
        return _json({'messages': []});
      case 'GET /v1/conversations/room:sera/library':
        return _json({'artifacts': []});
      case 'POST /v1/auth/logout':
        return _json({});
    }
    return _json({'error': 'unexpected $key'}, 404);
  }
}

Future<(OwnerSession, FakeServer, FakeDevice)> _signedIn(
  WidgetTester tester, {
  FakeServer? server,
  Size size = const Size(420, 900),
}) async {
  final srv = server ?? FakeServer();
  final dev = FakeDevice();
  final store = MemoryStore()
    ..endpoint = 'https://example.com'
    ..token = 'z' * 43;
  final session = _session(store, MockClient(srv.handle));
  await tester.binding.setSurfaceSize(size);
  addTearDown(() => tester.binding.setSurfaceSize(null));
  await tester.pumpWidget(SeraBotMobileApp(
    session: session,
    theme: ThemeController(prefs: MemoryThemePrefs()),
    device: dev,
    startEvents: false,
  ));
  await tester.pumpAndSettle();
  return (session, srv, dev);
}

void main() {
  testWidgets('shows Microsoft sign-in and no token entry', (tester) async {
    final session = _session(
      MemoryStore(),
      MockClient((_) async => http.Response('{}', 200)),
    );
    await tester.pumpWidget(SeraBotMobileApp(session: session));
    await tester.pumpAndSettle();
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
    expect(find.text('Server URL'), findsOneWidget);
    expect(find.textContaining('token'), findsNothing);
    expect(find.text('Invite code'), findsNothing);
    await tester.tap(find.text('I have an invite code'));
    await tester.pump();
    expect(find.text('Invite code'), findsOneWidget);
  });

  testWidgets('stored token that gets 401 falls back to sign-in', (tester) async {
    final store = MemoryStore()
      ..endpoint = 'https://example.com'
      ..token = 'z' * 43;
    final session = _session(
      store,
      MockClient((_) async => http.Response('{}', 401)),
    );
    await tester.pumpWidget(SeraBotMobileApp(session: session));
    await tester.pumpAndSettle();
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
    expect(store.token, isNull);
  });

  testWidgets('chat list shows the same conversations as the desktop', (tester) async {
    final (_, srv, _) = await _signedIn(tester);
    expect(srv.requests, contains('GET /v1/conversations'));
    expect(find.text('Daily plan'), findsOneWidget);
    expect(find.text('Here is your plan'), findsOneWidget);
    expect(find.text('Tax 2026'), findsOneWidget);
    expect(find.text('Finance'), findsOneWidget); // section header
    expect(find.text('Review team'), findsOneWidget);
    expect(find.text('2'), findsWidgets); // unread chip and tab badge
    // The old hard-coded conversation is never requested.
    expect(srv.requests.any((r) => r.contains('android-owner')), isFalse);
  });

  testWidgets('search filters the chat list', (tester) async {
    await _signedIn(tester);
    await tester.tap(find.byTooltip('Search'));
    await tester.pump();
    await tester.enterText(find.byType(TextField), 'tax');
    await tester.pump();
    expect(find.text('Tax 2026'), findsOneWidget);
    expect(find.text('Daily plan'), findsNothing);
  });

  testWidgets('opening a chat loads its messages and renders markdown', (tester) async {
    final (_, srv, _) = await _signedIn(tester);
    await tester.tap(find.text('Daily plan'));
    await tester.pumpAndSettle();
    expect(srv.requests, contains('GET /v1/conversations/dir:main/messages'));
    expect(find.text('Plan my day'), findsOneWidget);
    expect(find.textContaining('your', findRichText: true), findsWidgets);
    expect(find.text('Sera'), findsWidgets); // bot name above the reply
    expect(find.byKey(const Key('composer-field')), findsOneWidget);
  });

  testWidgets('composer sends text with an attachment', (tester) async {
    final (_, srv, dev) = await _signedIn(tester);
    dev.files = [const PickedFile('report.pdf', [1, 2, 3])];
    await tester.tap(find.text('Daily plan'));
    await tester.pumpAndSettle();
    expect(tester.widget<IconButton>(find.byKey(const Key('send-button'))).onPressed, isNull);
    await tester.tap(find.byKey(const Key('attach-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('File'));
    await tester.pumpAndSettle();
    expect(srv.requests, contains('POST /v1/conversations/dir:main/attachments'));
    expect(find.text('report.pdf'), findsOneWidget);
    await tester.enterText(find.byKey(const Key('composer-field')), 'summarise this');
    await tester.pump();
    await tester.tap(find.byKey(const Key('send-button')));
    await tester.pump();
    final body = srv.bodies['POST /v1/messages'] as Map<String, dynamic>;
    expect(body['conversation_id'], 'dir:main');
    expect(body['text'], 'summarise this');
    expect(body['attachments'], ['att-1']);
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.byKey(const Key('stop-button')), findsOneWidget); // turn is running
    expect(find.byKey(const Key('activity-panel')), findsOneWidget);
    await tester.pumpWidget(const SizedBox()); // dispose timers
  });

  testWidgets('mic button records and puts the transcript in the composer', (tester) async {
    final (_, srv, dev) = await _signedIn(tester);
    await tester.tap(find.text('Daily plan'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('mic-button')));
    await tester.pump();
    expect(dev.recording, isTrue);
    await tester.tap(find.byKey(const Key('mic-button')));
    await tester.pumpAndSettle();
    expect(srv.requests, contains('POST /v1/audio/transcribe'));
    expect(find.text('hello from voice'), findsOneWidget);
  });

  testWidgets('model chip shows the server error text', (tester) async {
    final srv = FakeServer()..modelError = 'prov needs data-egress approval';
    await _signedIn(tester, server: srv);
    await tester.tap(find.text('Daily plan'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('model-chip')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('prov'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('m1'));
    await tester.pumpAndSettle();
    expect(find.text('prov needs data-egress approval'), findsOneWidget);
  });

  testWidgets('approvals tab has a badge and decides through the API', (tester) async {
    final (_, srv, _) = await _signedIn(tester);
    expect(find.text('1'), findsWidgets); // badge
    await tester.tap(find.text('Approvals'));
    await tester.pumpAndSettle();
    expect(find.text('Send the report'), findsOneWidget);
    await tester.tap(find.text('Deny'));
    await tester.pumpAndSettle();
    expect(srv.bodies['POST /v1/approvals/ap1/decide'], {'decision': 'deny'});
  });

  testWidgets('bots tab lists the bots', (tester) async {
    await _signedIn(tester);
    await tester.tap(find.text('Bots'));
    await tester.pumpAndSettle();
    expect(find.text('Sera'), findsWidgets);
  });

  testWidgets('settings: account, connectors, usage, theme, update, sign out', (tester) async {
    final (_, srv, _) = await _signedIn(tester, size: const Size(420, 2400));
    await tester.tap(find.text('Settings'));
    await tester.pumpAndSettle();
    expect(find.text('Ada'), findsOneWidget);
    expect(find.text('Connect on desktop'), findsOneWidget);
    expect(find.text('34,567'), findsOneWidget);
    await tester.tap(find.text('Dark'));
    await tester.pumpAndSettle();
    expect(Theme.of(tester.element(find.byType(Scaffold).first)).brightness, Brightness.dark);
    await tester.tap(find.byKey(const Key('sign-out')));
    await tester.pumpAndSettle();
    // Signing out asks first; nothing is sent until the user confirms.
    expect(find.text('Sign out?'), findsOneWidget);
    expect(srv.requests, isNot(contains('POST /v1/auth/logout')));
    await tester.tap(find.byKey(const Key('confirm-action')));
    await tester.pumpAndSettle();
    expect(srv.requests, contains('POST /v1/auth/logout'));
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
  });

  testWidgets('new chat sheet lists bots and creates a group with them', (tester) async {
    final (_, srv, _) = await _signedIn(tester);
    await tester.tap(find.text('New chat'));
    await tester.pumpAndSettle();
    expect(find.text('Talk to'), findsOneWidget);
    await tester.tap(find.byType(CheckboxListTile).first);
    await tester.pump();
    expect(find.text('Start group with 1'), findsOneWidget);
    await tester.tap(find.text('Start group with 1'));
    await tester.pumpAndSettle();
    expect(srv.bodies['POST /v1/groups'], {'name': 'Sera', 'members': ['main']});
  });
}