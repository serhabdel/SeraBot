import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/mail_models.dart';
import 'package:serabot_mobile/screens/mail_screen.dart';

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
    var requests = 0;

    Future<ApiClient> stubClient() async {
      requests = 0;
      final mockHttp = MockClient((request) async {
        requests++;
        final path = request.url.path;
        if (path == '/v1/conversations/search' &&
            request.url.queryParameters['q'] == 'invoice') {
          return http.Response(
            jsonEncode({
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
            }),
            200,
          );
        } else if (path == '/v1/mail' && request.method == 'GET') {
          return http.Response(
            jsonEncode({
              'connected': true,
              'smtp_host': 'smtp.hostinger.com',
              'imap_host': 'imap.hostinger.com',
              'username': 'user@example.test',
            }),
            200,
          );
        } else if (path == '/v1/mail/connect' &&
            request.method == 'POST') {
          final payload = jsonDecode(request.body) as Map<String, dynamic>;
          expect(payload['smtp_host'], 'smtp.hostinger.com');
          expect(payload['smtp_security'], 'starttls');
          expect(payload['imap_security'], 'ssl');
          expect(payload['password'], 'secret');
          return http.Response(
            jsonEncode({
              'connected': true,
              'smtp_host': 'smtp.hostinger.com',
              'imap_host': 'imap.hostinger.com',
              'username': 'user@example.test',
              'smtp_test': 'ok',
              'imap_test': {'error': 'authentication failed'},
            }),
            200,
          );
        } else if (path == '/v1/mail' && request.method == 'DELETE') {
          return http.Response(jsonEncode({'connected': false}), 200);
        }
        return http.Response('', 404);
      });
      final client = ApiClient.connect(
        'http://127.0.0.1:8080',
        'c' * 32,
        httpClient: mockHttp,
        getRetryDelays: const [],
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

  group('mail message models', () {
    test('parses flags, attachments, and updates flags locally', () {
      final msg = MailMessageInfo.fromJson({
        'uid': 7,
        'subject': 'Quarterly report',
        'from': 'cfo@example.test',
        'date': '2026-11-24',
        'flags': [r'\Seen'],
        'body': 'Attached.',
        'attachments': [
          {
            'name': 'q4.pdf',
            'content_type': 'application/pdf',
            'size': 1024,
          },
        ],
      });
      expect(msg.isSeen, isTrue);
      expect(msg.isFlagged, isFalse);
      expect(msg.attachments.single.name, 'q4.pdf');

      final starred = msg.withUpdatedFlags(flagged: true, seen: false);
      expect(starred.isFlagged, isTrue);
      expect(starred.isSeen, isFalse);
    });
  });

  group('MailScreen interactive parity', () {
    testWidgets(
        'search, star/read/delete, save attachment to library, and compose/reply',
        (tester) async {
      final flagCalls = <Map<String, dynamic>>[];
      final deleteCalls = <int>[];
      final sendCalls = <Map<String, dynamic>>[];
      var savedAttachmentCalls = 0;

      final mockHttp = MockClient((req) async {
        final path = req.url.path;
        final method = req.method;
        if (path == '/v1/mail' && method == 'GET') {
          return http.Response(
            jsonEncode({
              'connected': true,
              'smtp_host': 'smtp.hostinger.com',
              'imap_host': 'imap.hostinger.com',
              'username': 'me@example.test',
              'accounts': [
                {
                  'account': '',
                  'smtp_host': 'smtp.hostinger.com',
                  'imap_host': 'imap.hostinger.com',
                  'username': 'me@example.test',
                },
              ],
            }),
            200,
          );
        }
        if (path == '/v1/mail/folders' && method == 'GET') {
          return http.Response(
            jsonEncode({'folders': ['INBOX', 'Sent']}),
            200,
          );
        }
        if (path == '/v1/mail/messages' && method == 'GET') {
          return http.Response(
            jsonEncode({
              'messages': [
                {
                  'uid': 101,
                  'subject': 'Contract review',
                  'from': 'legal@example.test',
                  'date': '2026-11-24',
                  'flags': <String>[],
                },
                {
                  'uid': 102,
                  'subject': 'Weekly standup',
                  'from': 'team@example.test',
                  'date': '2026-11-23',
                  'flags': [r'\Seen'],
                },
              ],
            }),
            200,
          );
        }
        if (path == '/v1/mail/search' && method == 'GET') {
          expect(req.url.queryParameters['q'], 'Contract');
          return http.Response(
            jsonEncode({
              'messages': [
                {
                  'uid': 101,
                  'subject': 'Contract review',
                  'from': 'legal@example.test',
                  'date': '2026-11-24',
                  'flags': <String>[],
                },
              ],
            }),
            200,
          );
        }
        if (path == '/v1/mail/messages/101' && method == 'GET') {
          return http.Response(
            jsonEncode({
              'message': {
                'uid': 101,
                'subject': 'Contract review',
                'from': 'legal@example.test',
                'date': '2026-11-24',
                'flags': <String>[],
                'body': 'Please review the attached contract.',
                'attachments': [
                  {
                    'name': 'contract.pdf',
                    'content_type': 'application/pdf',
                    'size': 4096,
                  },
                ],
              },
            }),
            200,
          );
        }
        if (path == '/v1/mail/messages/101/flag' && method == 'POST') {
          flagCalls.add(jsonDecode(req.body) as Map<String, dynamic>);
          return http.Response(jsonEncode({'ok': true}), 200);
        }
        if (path == '/v1/mail/messages/102' && method == 'DELETE') {
          deleteCalls.add(102);
          return http.Response(jsonEncode({'deleted': true}), 200);
        }
        if (path == '/v1/mail/messages/101/attachments/0/save-to-library' &&
            method == 'POST') {
          savedAttachmentCalls++;
          return http.Response(
            jsonEncode({
              'item': {'id': 'a' * 64, 'filename': 'contract.pdf'},
            }),
            200,
          );
        }
        if (path == '/v1/mail/send' && method == 'POST') {
          sendCalls.add(jsonDecode(req.body) as Map<String, dynamic>);
          return http.Response(jsonEncode({'sent': true}), 200);
        }
        return http.Response('', 404);
      });

      final client = ApiClient.connect(
        'https://example.com',
        't' * 32,
        httpClient: mockHttp,
        getRetryDelays: const [],
      );
      final app = AppState(client);
      addTearDown(app.dispose);
      addTearDown(client.close);

      await tester.pumpWidget(
        MaterialApp(home: MailScreen(app: app)),
      );
      await tester.pumpAndSettle();

      // Both messages visible
      expect(find.text('Contract review'), findsOneWidget);
      expect(find.text('Weekly standup'), findsOneWidget);

      // Server-side search filters to Contract review, clear restores both
      await tester.enterText(
        find.byKey(const Key('mail-search-input')),
        'Contract',
      );
      await tester.tap(find.byKey(const Key('mail-search-submit')));
      await tester.pumpAndSettle();
      expect(find.text('Contract review'), findsOneWidget);
      expect(find.text('Weekly standup'), findsNothing);

      await tester.tap(find.byKey(const Key('mail-search-clear')));
      await tester.pumpAndSettle();
      expect(find.text('Weekly standup'), findsOneWidget);

      // Star and mark read on message 101
      await tester.tap(find.byKey(const Key('mail-star-101')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('mail-read-101')));
      await tester.pumpAndSettle();
      expect(flagCalls, hasLength(2));
      expect(flagCalls[0]['flagged'], isTrue);
      expect(flagCalls[1]['seen'], isTrue);

      // Delete message 102 with confirmation dialog
      await tester.tap(find.byKey(const Key('mail-delete-102')));
      await tester.pumpAndSettle();
      expect(find.text('Delete message?'), findsOneWidget);
      await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
      await tester.pumpAndSettle();
      expect(deleteCalls, [102]);
      expect(find.text('Weekly standup'), findsNothing);

      // Open message 101 detail and save attachment to Library
      await tester.tap(find.byKey(const Key('mail-msg-101')));
      await tester.pumpAndSettle();
      expect(find.text('Please review the attached contract.'), findsOneWidget);
      expect(find.text('contract.pdf'), findsOneWidget);

      await tester.tap(find.byKey(const Key('mail-save-attachment-0')));
      await tester.pumpAndSettle();
      expect(savedAttachmentCalls, 1);
      expect(find.text('Saved "contract.pdf" to Library'), findsOneWidget);

      // Reply from detail opens Compose/Reply bottom sheet pre-filled
      await tester.tap(find.byKey(const Key('mail-detail-reply')));
      await tester.pumpAndSettle();
      expect(find.text('legal@example.test'), findsOneWidget);
      expect(find.text('Re: Contract review'), findsOneWidget);
      await tester.tap(find.byKey(const Key('mail-compose-send')));
      await tester.pumpAndSettle();
      expect(sendCalls, hasLength(1));
      expect(sendCalls.single['to'], ['legal@example.test']);
      expect(sendCalls.single['subject'], 'Re: Contract review');
    });
  });
  group('mail list fields (0.74.14)', () {
    test('parses snippet, thread key, attachment hint and unsubscribe', () {
      final msg = MailMessageInfo.fromJson({
        'uid': 7,
        'subject': 'Re: Plan',
        'from': 'Bob <bob@example.test>',
        'flags': ['Seen'],
        'snippet': 'Sounds good.',
        'thread_key': 't-1',
        'has_attachments': true,
        'unsubscribe': {
          'mailto': 'unsub@news.test?subject=unsubscribe',
          'url': 'https://news.test/u',
        },
      });
      expect(msg.snippet, 'Sounds good.');
      expect(msg.threadKey, 't-1');
      expect(msg.hasFiles, isTrue);
      expect(msg.unsubscribeMailto, 'unsub@news.test?subject=unsubscribe');
      expect(msg.unsubscribeUrl, 'https://news.test/u');
      // Changing flags keeps everything else.
      final copy = msg.copyWith(flags: const []);
      expect(copy.snippet, 'Sounds good.');
      expect(copy.threadKey, 't-1');
      expect(copy.unsubscribeUrl, 'https://news.test/u');
    });

    test('an older server omits the new fields', () {
      final msg = MailMessageInfo.fromJson({'uid': 1, 'subject': 'Old'});
      expect(msg.snippet, '');
      expect(msg.threadKey, '');
      expect(msg.hasFiles, isFalse);
      expect(msg.unsubscribeMailto, isNull);
    });
  });
}
