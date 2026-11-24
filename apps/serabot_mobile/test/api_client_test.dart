import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/bot_library_models.dart';
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

  test('0.74.9 libraryDocPreview parses slides and is null on 404', () async {
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      final kind = request.uri.queryParameters['kind'];
      if (request.uri.path == '/v1/library/preview-doc' && kind == 'pptx') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'kind': 'pptx',
            'slides': [
              {'index': 1, 'title': 'Intro', 'text': 'Hello'},
            ],
          }));
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'd' * 32);
    addTearDown(client.close);

    final deck = await client.libraryDocPreview('a' * 64, 'pptx');
    expect(deck!.slides.single.title, 'Intro');
    // A server without preview-doc answers 404: null, never a throw.
    expect(await client.libraryDocPreview('a' * 64, 'xlsx'), isNull);
    await expectLater(
      client.libraryDocPreview('not-a-hash', 'pptx'),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
    await expectLater(
      client.libraryDocPreview('a' * 64, 'docx'),
      throwsA(
        isA<ApiException>().having(
          (error) => error.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
  });

  test('0.74.9 libraryThumbnailBytes uses the artifact preview', () async {
    const pngB64 =
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC';
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      if (request.uri.path == '/v1/artifacts/${'a' * 64}/preview') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'content_type': 'image/png',
            'image_b64': pngB64,
          }));
      } else if (request.uri.path == '/v1/artifacts/${'b' * 64}') {
        // No preview for this one: the thumbnail falls back to the download.
        request.response
          ..statusCode = HttpStatus.ok
          ..add([1, 2, 3]);
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'e' * 32);
    addTearDown(client.close);

    BotLibraryItem item(String id, String filename, String mime) =>
        BotLibraryItem.fromJson({
          'kind': 'generated',
          'id': id,
          'tool': 'make_image',
          'filename': filename,
          'mime': mime,
          'conversation_id': 'dir:synthetic',
          'created_at': '2026-11-20T08:00:00Z',
        })!;

    final thumb = await client.libraryThumbnailBytes(
      item('a' * 64, 'photo.png', 'image/png'),
    );
    expect(thumb, base64Decode(pngB64));

    final fallback = await client.libraryThumbnailBytes(
      item('b' * 64, 'photo.png', 'image/png'),
    );
    expect(fallback, [1, 2, 3]);

    expect(
      await client.libraryThumbnailBytes(
        item('a' * 64, 'report.pdf', 'application/pdf'),
      ),
      isNull,
    );
  });

  test('getGlobalLibrary and uploadToLibrary round-trip and validate inputs', () async {
    String? uploadedName;
    List<int>? uploadedBody;
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = server!.port;
    server!.listen((request) async {
      if (request.uri.path == '/v1/library' && request.method == 'GET') {
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'enabled': true,
            'onedrive': {
              'mirror': true,
              'folder': 'SeraBot Library',
              'connected': true,
            },
            'items': [
              {
                'kind': 'generated',
                'id': 'a' * 64,
                'tool': 'deck_build',
                'name': 'Quarterly deck',
                'filename': 'deck.pptx',
                'mime': 'application/vnd.openxmlformats',
                'size_bytes': 4096,
                'conversation_id': 'dir:main',
                'conversation_title': 'Main chat',
                'created_at': '2026-11-20T08:00:00Z',
              },
              {
                'kind': 'uploaded',
                'id': 'b' * 64,
                'tool': 'library_upload',
                'name': 'notes.md',
                'filename': 'notes.md',
                'mime': 'text/plain; charset=utf-8',
                'size_bytes': 12,
                'conversation_id': 'library',
                'created_at': '2026-11-21T08:00:00Z',
              },
            ],
          }));
      } else if (request.uri.path == '/v1/library/upload' &&
          request.method == 'POST') {
        uploadedName = request.headers.value('x-filename');
        uploadedBody = await request.fold<List<int>>(
          <int>[],
          (acc, chunk) => acc..addAll(chunk),
        );
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'item': {
              'kind': 'uploaded',
              'id': 'c' * 64,
              'tool': 'library_upload',
              'name': uploadedName,
              'filename': uploadedName,
              'mime': 'text/plain',
              'size_bytes': uploadedBody!.length,
              'conversation_id': 'library',
              'created_at': '2026-11-22T08:00:00Z',
            },
            'onedrive': 'uploaded',
          }));
      } else {
        request.response.statusCode = HttpStatus.notFound;
      }
      await request.response.close();
    });

    final client = ApiClient.connect('http://127.0.0.1:$port', 'f' * 32);
    addTearDown(client.close);

    final lib = await client.getGlobalLibrary();
    expect(lib.enabled, isTrue);
    expect(lib.onedrive.mirror, isTrue);
    expect(lib.onedrive.connected, isTrue);
    expect(lib.onedrive.folder, 'SeraBot Library');
    expect(lib.items, hasLength(2));
    expect(lib.items.first.generated, isTrue);
    expect(lib.items.last.generated, isFalse);

    final reply = await client.uploadToLibrary('hello.txt', [104, 105]);
    expect(uploadedName, 'hello.txt');
    expect(uploadedBody, [104, 105]);
    expect(reply.onedrive, 'uploaded');
    expect(reply.item?.id, 'c' * 64);

    await expectLater(
      client.uploadToLibrary('empty.txt', const []),
      throwsA(
        isA<ApiException>().having(
          (e) => e.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
    await expectLater(
      client.uploadToLibrary('huge.bin', List<int>.filled(25 * 1024 * 1024 + 1, 1)),
      throwsA(
        isA<ApiException>().having(
          (e) => e.kind,
          'kind',
          ApiErrorKind.validation,
        ),
      ),
    );
  });

  test('needs_review task status parses and is not terminal', () {
    final task = TaskInfo.fromJson({
      'id': 't1',
      'kind': 'turn',
      'bot': 'main',
      'status': 'needs_review',
    });
    expect(task.status, TaskStatus.needsReview);
    expect(task.status.terminal, isFalse);
    expect(task.status.label, 'Needs review');
  });
}
