import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/mail_models.dart';

void main() {
  group('conversation search models', () {
    test('parses hits, skips rows without a conversation id', () {
      final hits = parseConversationSearchHits({
        'hits': [
          {
            'conversation_id': 'dir:main',
            'bot': 'main',
            'title': 'Main chat',
            'role': 'assistant',
            'snippet': 'Found invoice #42',
            'created_at': '2026-11-20T10:00:00Z',
          },
          {'bot': 'main', 'snippet': 'no id here'},
          {
            'conversation_id': 'dir:notes',
            'bot': 'notes',
            'title': '',
            'role': 'user',
            'snippet': 'invoice draft',
          },
        ],
      });
      expect(hits, hasLength(2));
      expect(hits.first.conversationId, 'dir:main');
      expect(hits.first.snippet, 'Found invoice #42');
      expect(hits.last.title, '');
    });

    test('non-list hits body parses as empty', () {
      expect(parseConversationSearchHits({}), isEmpty);
      expect(parseConversationSearchHits({'hits': 'nope'}), isEmpty);
    });
  });

  group('mail models', () {
    test('probe outcome: ok string, error object, bare string', () {
      expect(MailProbeOutcome.fromJson('ok').succeeded, isTrue);
      final err = MailProbeOutcome.fromJson({'error': 'authentication failed'});
      expect(err.succeeded, isFalse);
      expect(err.error, 'authentication failed');
      expect(
        MailProbeOutcome.fromJson('boom').error,
        'boom',
      );
      expect(MailProbeOutcome.fromJson(null).succeeded, isFalse);
    });

    test('connect response parses per-side test results', () {
      final resp = MailConnectResponse.fromJson({
        'connected': true,
        'smtp_host': 'smtp.hostinger.com',
        'imap_host': 'imap.hostinger.com',
        'username': 'user@example.test',
        'smtp_test': 'ok',
        'imap_test': {'error': 'authentication failed'},
      });
      expect(resp.connected, isTrue);
      expect(resp.smtpTest.succeeded, isTrue);
      expect(resp.imapTest.error, 'authentication failed');
    });

    test('status parses and defaults to not connected', () {
      expect(
        MailStatus.fromJson({'connected': true, 'username': 'u@x.test'})
            .connected,
        isTrue,
      );
      expect(MailStatus.fromJson({}).connected, isFalse);
    });

    test('validation: Hostinger preset passes, plain remote refused', () {
      final ok = MailConnectRequest.hostinger(
        username: 'me@example.test',
        password: 'secret',
      );
      expect(validateMailConnect(ok), isNull);
      expect(ok.toJson()['smtp_security'], 'starttls');
      expect(ok.toJson()['imap_security'], 'ssl');

      final plain = MailConnectRequest(
        smtpHost: 'smtp.hostinger.com',
        smtpPort: 587,
        smtpSecurity: MailSecurity.none,
        username: 'me@example.test',
        password: 'secret',
        imapHost: 'imap.hostinger.com',
        imapPort: 993,
        imapSecurity: MailSecurity.ssl,
      );
      expect(validateMailConnect(plain), contains('Plain SMTP'));

      final noPassword = MailConnectRequest.hostinger(
        username: 'me@example.test',
        password: '',
      );
      expect(validateMailConnect(noPassword), contains('Password'));
    });
  });

  group('search and mail against local stub', () {
    HttpServer? server;
    var requests = 0;

    tearDown(() async {
      await server?.close(force: true);
      server = null;
    });

    Future<ApiClient> stubClient() async {
      requests = 0;
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      final port = server!.port;
      server!.listen((request) async {
        requests++;
        final path = request.uri.path;
        if (path == '/v1/conversations/search' &&
            request.uri.queryParameters['q'] == 'invoice') {
          request.response
            ..statusCode = HttpStatus.ok
            ..write(jsonEncode({
              'hits': [
                {
                  'conversation_id': 'dir:main',
                  'bot': 'main',
                  'title': 'Main chat',
                  'role': 'assistant',
                  'snippet': 'Found invoice #42',
                  'created_at': '2026-11-20T10:00:00Z',
                },
              ],
            }));
        } else if (path == '/v1/mail' && request.method == 'GET') {
          request.response
            ..statusCode = HttpStatus.ok
            ..write(jsonEncode({
              'connected': true,
              'smtp_host': 'smtp.hostinger.com',
              'imap_host': 'imap.hostinger.com',
              'username': 'user@example.test',
            }));
        } else if (path == '/v1/mail/connect' &&
            request.method == 'POST') {
          final payload =
              jsonDecode(await utf8.decoder.bind(request).join())
                  as Map<String, dynamic>;
          expect(payload['smtp_host'], 'smtp.hostinger.com');
          expect(payload['smtp_security'], 'starttls');
          expect(payload['imap_security'], 'ssl');
          expect(payload['password'], 'secret');
          request.response
            ..statusCode = HttpStatus.ok
            ..write(jsonEncode({
              'connected': true,
              'smtp_host': 'smtp.hostinger.com',
              'imap_host': 'imap.hostinger.com',
              'username': 'user@example.test',
              'smtp_test': 'ok',
              'imap_test': {'error': 'authentication failed'},
            }));
        } else if (path == '/v1/mail' && request.method == 'DELETE') {
          request.response
            ..statusCode = HttpStatus.ok
            ..write(jsonEncode({'connected': false}));
        } else {
          request.response.statusCode = HttpStatus.notFound;
        }
        await request.response.close();
      });
      final client = ApiClient.connect(
        'http://127.0.0.1:$port',
        'c' * 32,
      );
      addTearDown(client.close);
      return client;
    }

    test('search hits, short query stays local, 404 reads as empty', () async {
      final client = await stubClient();
      final hits = await client.searchConversations(
        'invoice',
        bot: 'main',
        limit: 15,
      );
      expect(hits, hasLength(1));
      expect(hits.first.conversationId, 'dir:main');
      expect(hits.first.snippet, 'Found invoice #42');

      final before = requests;
      expect(await client.searchConversations('x'), isEmpty);
      expect(requests, before);

      // Unknown query: the stub answers 404, like a pre-0.74.2 server.
      expect(await client.searchConversations('unknown query'), isEmpty);
    });

    test('mail status, connect with live tests, disconnect', () async {
      final client = await stubClient();
      final status = await client.mailStatus();
      expect(status.connected, isTrue);
      expect(status.username, 'user@example.test');

      final resp = await client.mailConnect(
        MailConnectRequest.hostinger(
          username: 'user@example.test',
          password: 'secret',
        ),
      );
      expect(resp.connected, isTrue);
      expect(resp.smtpTest.succeeded, isTrue);
      expect(resp.imapTest.error, 'authentication failed');

      final gone = await client.mailDisconnect();
      expect(gone.connected, isFalse);
    });

    test('mail connect refuses plain remote hosts without a request',
        () async {
      final client = await stubClient();
      final before = requests;
      await expectLater(
        client.mailConnect(
          MailConnectRequest(
            smtpHost: 'smtp.hostinger.com',
            smtpPort: 587,
            smtpSecurity: MailSecurity.none,
            username: 'me@example.test',
            password: 'secret',
            imapHost: 'imap.hostinger.com',
            imapPort: 993,
            imapSecurity: MailSecurity.ssl,
          ),
        ),
        throwsA(isA<ApiException>()),
      );
      expect(requests, before);
    });
  });
}
