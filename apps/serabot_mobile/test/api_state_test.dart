import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/chat_controller.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/models.dart';

final token = 'z' * 43;

http.Response json(Object body, [int status = 200]) =>
    http.Response(jsonEncode(body), status,
        headers: {'content-type': 'application/json'});

ApiClient clientFor(
  Future<http.Response> Function(http.Request) handler, {
  void Function()? onUnauthorized,
}) =>
    ApiClient.connect('https://example.com', token,
        httpClient: MockClient(handler), onUnauthorized: onUnauthorized);

Map<String, dynamic> conv(String id, {String kind = 'direct', int unread = 0}) => {
      'id': id,
      'kind': kind,
      'created_at': 'a',
      'updated_at': 'b',
      'unread_count': unread,
      'accepts_turns': true,
    };

void main() {
  test('conversations: follows the cursor and sends the bearer token only', () async {
    final seen = <Uri>[];
    final c = clientFor((r) async {
      seen.add(r.url);
      expect(r.headers['Authorization'], 'Bearer $token');
      expect(r.followRedirects, isFalse);
      if (r.url.queryParameters['cursor'] == null) {
        return json({'conversations': [conv('dir:a')], 'next_cursor': 'c2'});
      }
      return json({'conversations': [conv('dir:b')]});
    });
    final list = await c.conversations();
    expect(list.map((e) => e.id), ['dir:a', 'dir:b']);
    expect(seen.first.path, '/v1/conversations');
    expect(seen.last.queryParameters['cursor'], 'c2');
  });

  test('redirect is an error, not followed', () async {
    final c = clientFor((r) async => http.Response('', 302, headers: {'location': 'https://evil.example'}));
    await expectLater(c.conversations(),
        throwsA(isA<ApiException>().having((e) => e.statusCode, 'status', 302)));
  });

  test('server error text is surfaced for 400/409 but not for 403', () async {
    final c = clientFor((r) async {
      if (r.method == 'PUT') {
        return json({'error': 'provider needs data-egress approval'}, 409);
      }
      return json({'error': 'secret'}, 403);
    });
    await expectLater(
      c.selectModel('dir:main', const Selection(provider: 'p', model: 'm')),
      throwsA(isA<ApiException>().having(
          (e) => e.messageForUser(), 'text', 'provider needs data-egress approval')),
    );
    await expectLater(c.bots(),
        throwsA(isA<ApiException>().having((e) => e.messageForUser(), 'text', contains('403'))));
  });

  test('401 calls onUnauthorized', () async {
    var hit = 0;
    final c = clientFor((r) async => json({}, 401), onUnauthorized: () => hit++);
    await expectLater(c.bots(), throwsA(isA<ApiException>()));
    expect(hit, 1);
  });

  test('upload sends raw bytes with plain X-Filename', () async {
    late http.Request req;
    final c = clientFor((r) async {
      req = r;
      return json({'attachment': {'id': 'att-1', 'filename': 'a.txt', 'size_bytes': 3}});
    });
    final a = await c.uploadAttachment('dir:main', 'a.txt', [1, 2, 3]);
    expect(a.id, 'att-1');
    expect(req.headers['X-Filename'], 'a.txt');
    expect(req.url.path, '/v1/conversations/dir:main/attachments');
    expect(req.bodyBytes, [1, 2, 3]);
    await expectLater(c.uploadAttachment('dir:main', 'a.exe', [1]), throwsA(isA<ApiException>()));
    await expectLater(c.uploadAttachment('dir:main', 'a.txt', []), throwsA(isA<ApiException>()));
  });

  test('submit includes attachments; empty text allowed only with files', () async {
    Map<String, dynamic>? body;
    final c = clientFor((r) async {
      body = jsonDecode(r.body) as Map<String, dynamic>;
      return json({'task': {'id': 't1', 'kind': 'turn', 'bot': 'main', 'status': 'queued'}});
    });
    await c.submit('dir:main', '', attachments: ['att-1']);
    expect(body!['attachments'], ['att-1']);
    await expectLater(c.submit('dir:main', ''), throwsA(isA<ApiException>()));
  });

  test('bot update uses PUT /v1/bots/{id} with prompt field', () async {
    late http.Request req;
    final c = clientFor((r) async {
      req = r;
      return json({'ok': true});
    });
    await c.updateBot('analyst', botSettingsBody(instructions: 'Short'));
    expect(req.method, 'PUT');
    expect(req.url.path, '/v1/bots/analyst');
    expect(jsonDecode(req.body), {'prompt': 'Short'});
  });

  test('avatar returns null unless the body is a PNG', () async {
    var png = true;
    final c = clientFor((r) async => http.Response.bytes(
        png ? [0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4, 5] : [1, 2, 3, 4, 5, 6, 7, 8, 9], 200));
    expect(await c.avatarBytes('bots', 'main'), isNotNull);
    png = false;
    expect(await c.avatarBytes('bots', 'main'), isNull);
  });

  test('library rejects a bad hash; artifact download validates hash', () async {
    final c = clientFor((r) async => json({
          'artifacts': [
            {'action_id': 'a', 'tool': 't', 'hash': 'nothex', 'created_at': 'x'}
          ]
        }));
    await expectLater(c.library('dir:main'), throwsA(isA<ApiException>()));
    await expectLater(c.artifactBytes('../x'), throwsA(isA<ApiException>()));
  });

  test('events stream yields parsed frames and 401 signs out', () async {
    final stream = Stream.fromIterable([
      utf8.encode(': hi\n\nid: 1\nevent: message\ndata: {"conversation_id":"dir:a"}\n\n'),
      utf8.encode('id: 2\nevent: turn.step\ndata: {"task_id":"t"}\n\n'),
    ]);
    final c = ApiClient.connect('https://example.com', token,
        httpClient: MockClient.streaming((req, body) async {
      expect(req.url.queryParameters['after'], '5');
      return http.StreamedResponse(stream, 200);
    }));
    final events = await c.events(after: 5).toList();
    expect(events.map((e) => e.kind), ['message', 'turn.step']);
    expect(events.first.conversationId, 'dir:a');

    var out = 0;
    final u = ApiClient.connect('https://example.com', token,
        httpClient: MockClient.streaming(
            (req, body) async => http.StreamedResponse(const Stream.empty(), 401)),
        onUnauthorized: () => out++);
    await expectLater(u.events().toList(), throwsA(isA<ApiException>()));
    expect(out, 1);
  });

  group('AppState', () {
    test('loads the same list the desktop sees and tracks unread + cursor', () async {
      final c = clientFor((r) async {
        switch (r.url.path) {
          case '/v1/conversations':
            return json({'conversations': [conv('dir:a', unread: 2), conv('room:b', kind: 'room', unread: 1)]});
          case '/v1/bots':
            return json({'bots': [{'id': 'main', 'display_name': 'Sera'}]});
          case '/v1/approvals':
            return json({'approvals': [
              {'id': 'ap1', 'action_id': 'act1', 'tool': 't', 'summary': 's', 'expires_at': 'x'}
            ]});
        }
        return json({}, 404);
      });
      final s = AppState(c, eventDebounce: Duration.zero);
      await s.refreshConversations();
      await s.refreshBots();
      await s.refreshApprovals();
      expect(s.conversations.map((e) => e.id), ['dir:a', 'room:b']);
      expect(s.unread, 3);
      expect(s.botById('main')!.name, 'Sera');
      expect(s.pendingApprovals, 1);
      s.markReadLocally('dir:a');
      expect(s.unread, 1);
      s.handleEvent(const ServerEvent(9, 'turn.step', {}));
      expect(s.cursor, 9);
      s.dispose();
    });

    test('an event refreshes the list (debounced)', () async {
      var calls = 0;
      final c = clientFor((r) async {
        if (r.url.path == '/v1/conversations') {
          calls++;
          return json({'conversations': [conv('dir:a', unread: calls)]});
        }
        return json({}, 404);
      });
      final s = AppState(c, eventDebounce: const Duration(milliseconds: 10));
      s.handleEvent(const ServerEvent(1, 'message', {'conversation_id': 'dir:a'}));
      s.handleEvent(const ServerEvent(2, 'message', {'conversation_id': 'dir:a'}));
      await Future<void>.delayed(const Duration(milliseconds: 80));
      expect(calls, 1);
      expect(s.conversations.single.unreadCount, 1);
      s.dispose();
    });
  });

  group('ChatController', () {
    test('send -> steps -> completion reloads the transcript', () async {
      var history = 0;
      final c = clientFor((r) async {
        final p = r.url.path;
        if (p == '/v1/conversations/dir:main/messages' && r.method == 'GET') {
          history++;
          return json({
            'messages': [
              {'id': 'm1', 'role': 'user', 'content': 'hi', 'created_at': 'a'},
              if (history > 1)
                {'id': 'm2', 'role': 'assistant', 'content': 'done', 'created_at': 'b', 'bot': 'main'},
            ]
          });
        }
        if (p == '/v1/messages') {
          return json({'task': {'id': 't1', 'kind': 'turn', 'bot': 'main', 'status': 'queued'}});
        }
        if (p.endsWith('/library')) {
          return json({'artifacts': []});
        }
        return json({}, 404);
      });
      final app = AppState(c, eventDebounce: Duration.zero);
      final chat = ChatController('dir:main', c, app, pollInterval: const Duration(hours: 1));
      await chat.load();
      expect(chat.messages.length, 1);
      expect(await chat.send('hello'), isTrue);
      expect(chat.running, isTrue);
      expect(chat.messages.last.content, 'hello');
      // another task's steps are ignored
      app.handleEvent(const ServerEvent(1, 'turn.step',
          {'task_id': 'old', 'call_id': 'x', 'label': 'Old', 'phase': 'running', 'conversation_id': 'dir:main'}));
      await Future<void>.delayed(Duration.zero);
      expect(chat.activity.isEmpty, isTrue);
      app.handleEvent(const ServerEvent(2, 'turn.step',
          {'task_id': 't1', 'call_id': 'c1', 'label': 'Searching', 'phase': 'running', 'conversation_id': 'dir:main'}));
      await Future<void>.delayed(Duration.zero);
      expect(chat.activity.steps.single.label, 'Searching');
      // a second send while running is refused
      expect(await chat.send('again'), isFalse);
      app.handleEvent(const ServerEvent(3, 'turn.completed', {'task_id': 't1', 'conversation_id': 'dir:main'}));
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(chat.running, isFalse);
      expect(chat.messages.last.content, 'done');
      chat.dispose();
      app.dispose();
    });

    test('server error text from selectModel is returned verbatim', () async {
      final c = clientFor((r) async => json({'error': 'model not allowed'}, 400));
      final app = AppState(c);
      final chat = ChatController('dir:main', c, app);
      final err = await chat.selectModel(const Selection(provider: 'p', model: 'm'));
      expect(err, 'model not allowed');
      chat.dispose();
      app.dispose();
    });

    test('attachments: add, limit, remove', () async {
      final c = clientFor((r) async {
        final n = r.headers['X-Filename']!;
        return json({'attachment': {'id': 'att-${n.split('.').first}', 'filename': n}});
      });
      final app = AppState(c);
      final chat = ChatController('dir:main', c, app);
      for (var i = 0; i < 7; i++) {
        await chat.addAttachment('f$i.txt', [1]);
      }
      expect(chat.attachments.length, maxAttachments);
      expect(chat.error, contains('at most'));
      chat.removeAttachment(chat.attachments.first);
      expect(chat.attachments.length, maxAttachments - 1);
      chat.dispose();
      app.dispose();
    });
  });
}

