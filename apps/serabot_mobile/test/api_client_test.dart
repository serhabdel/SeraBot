import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/api_client.dart';

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
}
