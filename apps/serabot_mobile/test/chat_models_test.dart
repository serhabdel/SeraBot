import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/chat_models.dart';
import 'package:serabot_mobile/models.dart';

Map<String, dynamic> conv(
  String id,
  String kind, {
  String? parent,
  String? title,
  int unread = 0,
  Map<String, dynamic>? last,
}) =>
    {
      'id': id,
      'kind': kind,
      'parent_id': parent,
      'title': title,
      'created_at': '2026-01-01T00:00:00Z',
      'updated_at': '2026-01-02T00:00:00Z',
      'unread_count': unread,
      'accepts_turns': kind == 'direct' || kind == 'room',
      'last_message': last,
    };

void main() {
  group('conversations', () {
    test('parse a page with title, unread, last message, legacy kind', () {
      final page = ConversationPage.fromJson({
        'conversations': [
          conv('dir:main', 'direct',
              title: 'Daily', unread: 3, last: {'role': 'assistant', 'text': 'hi'}),
          conv('windows-owner', 'legacy'),
        ],
        'next_cursor': 'abc',
      }, 100);
      expect(page.conversations.first.displayTitle, 'Daily');
      expect(page.conversations.first.unreadCount, 3);
      expect(page.conversations.first.lastMessage!.text, 'hi');
      expect(page.conversations[1].kind, 'direct');
      expect(page.conversations[1].displayTitle, 'windows-owner');
      expect(page.nextCursor, 'abc');
    });

    test('rejects a page larger than the limit', () {
      expect(
        () => ConversationPage.fromJson({
          'conversations': [conv('a', 'direct'), conv('b', 'direct')],
        }, 1),
        throwsFormatException,
      );
    });

    test('buckets keep server order, nest under folders, filter by search', () {
      final all = [
        ConversationSummary.fromJson(conv('sec:fin', 'section', title: 'Finance')),
        ConversationSummary.fromJson(conv('room:b', 'room', title: 'Budget', unread: 2)),
        ConversationSummary.fromJson(conv('dir:main', 'direct', title: 'Main')),
        ConversationSummary.fromJson(conv('dir:tax', 'direct', parent: 'sec:fin', title: 'Tax')),
        ConversationSummary.fromJson(conv('dir:inv', 'direct', parent: 'sec:fin', title: 'Invoices', unread: 1)),
      ];
      final b = bucketConversations(all);
      expect(b.first.folder, isNull);
      expect(b.first.chats.map((c) => c.id), ['room:b', 'dir:main']);
      expect(b[1].folder!.id, 'sec:fin');
      expect(b[1].chats.map((c) => c.id), ['dir:tax', 'dir:inv']);
      expect(b[1].unread, 1);
      expect(totalUnread(all), 3);
      final f = bucketConversations(all, query: 'inv');
      expect(f.length, 1);
      expect(f.single.chats.single.id, 'dir:inv');
    });

    test('a chat whose folder is missing falls back to the top level', () {
      final b = bucketConversations([
        ConversationSummary.fromJson(conv('dir:x', 'direct', parent: 'sec:gone')),
      ]);
      expect(b.single.folder, isNull);
      expect(b.single.chats.single.id, 'dir:x');
    });
  });

  group('messages', () {
    test('parse bot, name and attachments', () {
      final m = ChatMessage.fromJson({
        'id': 'm1',
        'role': 'assistant',
        'content': 'ok',
        'created_at': 'now',
        'bot': 'analyst',
        'name': 'analyst',
        'attachments': [
          {'id': 'att-1', 'filename': 'a.pdf', 'mime': 'application/pdf', 'size_bytes': 5, 'is_image': false}
        ],
      });
      expect(m.speaker, 'analyst');
      expect(m.attachments.single.filename, 'a.pdf');
      final plain = ChatMessage.fromJson(
          {'id': 'm2', 'role': 'user', 'content': 'x', 'created_at': 'n'});
      expect(plain.speaker, isNull);
      expect(plain.attachments, isEmpty);
    });
  });

  group('attachments', () {
    test('allowlist and header-safe names', () {
      expect(validAttachmentName('report.PDF'), isTrue);
      expect(validAttachmentName('x.exe'), isFalse);
      expect(validAttachmentName('a/b.txt'), isFalse);
      expect(validAttachmentName('noext'), isFalse);
      expect(safeUploadName('/sdcard/DCIM/photo été.jpg'), 'photo _t_.jpg');
    });
  });

  group('events', () {
    test('SSE parser handles split chunks, comments and ids', () {
      final p = SseParser();
      expect(p.feed(': keepalive\n\nid: 7\nevent: turn.st'), isEmpty);
      final frames = p.feed('ep\ndata: {"a":1}\n\n');
      expect(frames.length, 1);
      expect(frames.single.id, 7);
      expect(frames.single.event, 'turn.step');
      expect(frames.single.data, '{"a":1}');
      final multi = p.feed('id: 8\ndata: x\ndata: y\n\n');
      expect(multi.single.data, 'x\ny');
    });

    test('SSE parser drops an oversized buffer instead of growing', () {
      final p = SseParser();
      expect(p.feed('data: ${'a' * (SseParser.maxBuffer + 1)}'), isEmpty);
      expect(p.feed('id: 1\ndata: ok\n\n').single.data, 'ok');
    });

    test('frame without id is ignored, bad json becomes empty payload', () {
      expect(ServerEvent.fromFrame(const SseFrame(null, 'x', '{}')), isNull);
      final e = ServerEvent.fromFrame(const SseFrame(3, 'message', 'nope'))!;
      expect(e.payload, isEmpty);
      expect(e.kind, 'message');
    });

    test('activity builds steps per task, updates in place, bounds text', () {
      final a = Activity();
      Map<String, dynamic> ev(String call, String phase, {String? approval}) => {
            'task_id': 't1',
            'call_id': call,
            'label': 'Reading\u0007 file',
            'phase': phase,
            'approval_id': ?approval,
          };
      expect(a.apply('turn.completed', ev('c1', 'running')), isFalse);
      expect(a.apply('turn.step', ev('c1', 'running')), isTrue);
      expect(a.steps.single.label, 'Reading file');
      expect(a.apply('turn.step', ev('c1', 'waiting_approval', approval: 'ap1')), isTrue);
      expect(a.steps.single.approvalId, 'ap1');
      a.apply('turn.step', ev('c1', 'done'));
      expect(a.steps.single.approvalId, isNull);
      a.apply('turn.step', ev('c2', 'running'));
      expect(a.summary, '2 steps');
      a.apply('turn.step', {...ev('c3', 'running'), 'task_id': 't2'});
      expect(a.steps.length, 1);
      expect(a.apply('turn.step', {'task_id': 't2'}), isFalse);
    });
  });

  group('bots, usage, connectors', () {
    test('bot summary and detail parse with defaults', () {
      final b = BotSummary.fromJson({'id': 'main', 'display_name': ' ', 'tools': ['web_search']});
      expect(b.name, 'main');
      expect(b.enabled, isTrue);
      final d = BotDetail.fromJson({'id': 'main', 'display_name': 'Sera', 'model': 'x', 'instructions': 'hi'});
      expect(d.displayName, 'Sera');
      expect(d.instructions, 'hi');
      expect(d.tools, isEmpty);
    });

    test('bot settings body sends prompt, only changed fields', () {
      expect(botSettingsBody(), isEmpty);
      final body = botSettingsBody(
        displayName: ' Ana ',
        instructions: 'Be brief',
        model: (provider: 'p', model: 'm'),
        tools: ['a'],
      );
      expect(jsonEncode(body),
          '{"display_name":"Ana","prompt":"Be brief","model":{"provider":"p","model":"m"},"tools":["a"]}');
      expect(botSettingsBody(clearModel: true), {'model': null});
    });

    test('connector status labels', () {
      ConnectorStatus c(Map<String, dynamic> j) =>
          ConnectorStatus.fromJson({'id': 'x', 'kind': 'k', ...j});
      expect(c({'personal': {'state': 'connected', 'account': 'a@b'}}).statusLabel, 'Connected');
      final ready = c({'personal': {'state': 'ready'}});
      expect(ready.statusLabel, 'Ready to connect');
      expect(ready.needsDesktop, isTrue);
      expect(c({'personal': {'state': 'not_set_up'}}).statusLabel, 'Not set up');
      expect(c({'enabled': true}).statusLabel, 'On');
      expect(c({'connected': true}).isConnected, isTrue);
    });

    test('usage report parses totals and per-model rows', () {
      final u = UsageReport.fromJson({
        'totals': {'calls': 2, 'total_tokens': 30},
        'per_model': [
          {'provider': 'p', 'model': 'm', 'calls': 2, 'total_tokens': 30}
        ],
      });
      expect(u.totals.totalTokens, 30);
      expect(u.perModel.single.model, 'm');
    });
  });
}
