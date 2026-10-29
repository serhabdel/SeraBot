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

  test('create and list skills without a path or echoed body', () async {
    final recorded = <String, String>{};
    final methods = <String>[];
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      final payload = await utf8.decoder.bind(request).join();
      methods.add('${request.method} ${request.uri.path}');
      recorded['${methods.length}'] = payload;
      if (request.uri.path == '/v1/skills' && request.method == 'POST') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'skill': {
              'id': 'notes-pack',
              'name': 'Notes Pack',
              'description': 'Short summary',
              'file': 'skills/notes-pack/SKILL.md',
              'live': false,
            },
          }));
      } else if (request.uri.path == '/v1/skills' && request.method == 'GET') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'skills': [
              {'id': 'ask', 'name': 'Ask', 'description': 'Synthetic summary'},
              {
                'id': 'notes-pack',
                'name': 'Notes Pack',
                'description': 'Short summary',
                'pack': 'office',
              },
            ],
          }));
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'd' * 32);
    addTearDown(client.close);

    final skill = await client.createSkill(
      'notes-pack',
      '  Short summary  ',
      '  # Notes\n\nUse allowlisted tools.\n  ',
    );
    expect(skill.id, 'notes-pack');
    expect(skill.live, isFalse);
    expect(skill.file, 'skills/notes-pack/SKILL.md');
    final created = jsonDecode(recorded['1']!) as Map<String, dynamic>;
    expect(created, {
      'id': 'notes-pack',
      'description': 'Short summary',
      'body': '# Notes\n\nUse allowlisted tools.',
    });
    expect(created.containsKey('path'), isFalse);

    final stub = await client.createSkill('notes-pack', 'Short summary', '   ');
    expect(stub.live, isFalse);
    final sent = jsonDecode(recorded['2']!) as Map<String, dynamic>;
    expect(sent.containsKey('body'), isFalse);
    expect(sent.containsKey('path'), isFalse);
    expect(sent, {'id': 'notes-pack', 'description': 'Short summary'});

    final skills = await client.listSkills();
    expect(skills, hasLength(2));
    expect(skills[1].pack, 'office');
    expect(methods, ['POST /v1/skills', 'POST /v1/skills', 'GET /v1/skills']);
  });

  test('skill responses must stay offline and relative', () async {
    final bodies = <String>[
      jsonEncode({
        'skill': {
          'id': 'notes-pack',
          'name': 'Notes Pack',
          'description': 'Short summary',
          'file': 'skills/notes-pack/SKILL.md',
          'live': true,
        },
      }),
      jsonEncode({
        'skill': {
          'id': 'other',
          'name': 'Other',
          'description': 'Short summary',
          'file': 'skills/other/SKILL.md',
          'live': false,
        },
      }),
      jsonEncode({
        'skill': {
          'id': 'notes-pack',
          'name': 'Notes Pack',
          'description': 'Short summary',
          'file': '../notes-pack/SKILL.md',
          'live': false,
        },
      }),
      jsonEncode({
        'skill': {
          'id': 'notes-pack',
          'name': 'Notes Pack',
          'description': 'Short summary',
          'file': 'C:/skills/notes-pack/SKILL.md',
          'live': false,
        },
      }),
      jsonEncode({
        'skill': {
          'id': 'notes-pack',
          'name': '../Notes',
          'description': 'Short summary',
          'file': 'skills/notes-pack/SKILL.md',
          'live': false,
        },
      }),
      jsonEncode({
        'skills': [
          {'id': '../ask', 'name': 'Ask', 'description': 'Synthetic summary'},
        ],
      }),
    ];
    var index = 0;
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      request.response
        ..statusCode = HttpStatus.ok
        ..write(bodies[index]);
      index += 1;
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'e' * 32);
    addTearDown(client.close);

    await expectLater(
      client.createSkill('con', 'Short summary', null),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
    expect(index, 0);

    for (var n = 0; n < 5; n++) {
      await expectLater(
        client.createSkill('notes-pack', 'Short summary', null),
        throwsA(
          isA<ApiException>().having(
            (error) => error.kind,
            'kind',
            ApiErrorKind.invalidResponse,
          ),
        ),
      );
      expect(index, n + 1);
    }
    await expectLater(
      client.listSkills(),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.invalidResponse,
        ),
      ),
    );
  });
}
