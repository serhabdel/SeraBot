import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/models.dart';

void main() {
  HttpServer? server;

  tearDown(() async {
    await server?.close(force: true);
    server = null;
  });

  test('health and history against local stub', () async {
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      if (request.uri.path == '/v1/health') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({'ok': true, 'version': '0.test'}));
      } else if (request.uri.path == '/v1/conversations/c1/messages') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'messages': [
              {
                'id': 'm1',
                'role': 'user',
                'content': 'hi',
                'created_at': '2026-01-01T00:00:00Z',
              },
            ],
          }));
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final token = 'a' * 32;
    final client = ApiClient.connect('http://127.0.0.1:$port', token);
    addTearDown(client.close);

    final health = await client.health();
    expect(health.ok, isTrue);
    expect(health.version, '0.test');

    final messages = await client.history('c1');
    expect(messages, hasLength(1));
    expect(messages.first.content, 'hi');
  });

  test('create bot and conversation against local stub', () async {
    final bodies = <String, String>{};
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      final payload = await utf8.decoder.bind(request).join();
      bodies[request.uri.path] = payload;
      if (request.uri.path == '/v1/bots' && request.method == 'POST') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'bot': {
              'id': 'notes',
              'enabled': false,
              'role': 'specialist',
              'live': false,
              'tools': ['skill_list', 'rag_search'],
              'skills': ['ask'],
              'require_approval': <String>[],
              'bot_file': 'bots/notes.toml',
              'prompt_file': 'prompts/notes.txt',
            },
          }));
      } else if (request.uri.path == '/v1/conversations' && request.method == 'POST') {
        final sent = jsonDecode(payload) as Map<String, dynamic>;
        final kind = sent['kind'] as String;
        final slug = sent['slug'] as String;
        final prefix = switch (kind) {
          'section' => 'sec',
          'group' => 'grp',
          'direct' => 'dir',
          'room' => 'room',
          _ => 'bad',
        };
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'conversation': {
              'id': '$prefix:$slug',
              'kind': kind,
              'parent_id': sent['parent_id'],
              'accepts_turns': kind == 'direct' || kind == 'room',
            },
          }));
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final token = 'b' * 32;
    final client = ApiClient.connect('http://127.0.0.1:$port', token);
    addTearDown(client.close);

    final bot = await client.createBot(
      'notes',
      'Synthetic notes specialist.',
      ['skill_list', 'rag_search'],
      ['ask'],
    );
    expect(bot.id, 'notes');
    expect(bot.enabled, isFalse);
    expect(bot.live, isFalse);
    expect(jsonDecode(bodies['/v1/bots']!), {
      'id': 'notes',
      'description': 'Synthetic notes specialist.',
      'tools': ['skill_list', 'rag_search'],
      'skills': ['ask'],
    });

    final section = await client.createConversation(
      ConversationKind.section,
      'finance',
      null,
    );
    expect(section.id, 'sec:finance');
    expect(section.acceptsTurns, isFalse);
    expect(jsonDecode(bodies['/v1/conversations']!), {
      'kind': 'section',
      'slug': 'finance',
    });

    final group = await client.createConversation(
      ConversationKind.group,
      'ops',
      'sec:finance',
    );
    expect(group.id, 'grp:ops');
    expect(group.parentId, 'sec:finance');
    expect(jsonDecode(bodies['/v1/conversations']!), {
      'kind': 'group',
      'slug': 'ops',
      'parent_id': 'sec:finance',
    });
  });

  test('createBot rejects a live scaffold and a reserved id', () async {
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      request.response
        ..statusCode = HttpStatus.ok
        ..write(jsonEncode({
          'bot': {
            'id': 'notes',
            'enabled': true,
            'role': 'specialist',
            'live': false,
            'tools': <String>[],
            'skills': <String>[],
            'require_approval': <String>[],
            'bot_file': 'bots/notes.toml',
            'prompt_file': 'prompts/notes.txt',
          },
        }));
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'c' * 32);
    addTearDown(client.close);

    await expectLater(
      client.createBot('main', '', const [], const []),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
    await expectLater(
      client.createBot('notes', '', const [], const []),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.invalidResponse,
        ),
      ),
    );
    await expectLater(
      client.createConversation(ConversationKind.group, 'ops', null),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
  });
}
