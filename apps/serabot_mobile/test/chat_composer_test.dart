import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/chat_controller.dart';

const _chat = 'dir:composer';

ChatController _chatFor({DraftStore? store}) {
  final client = ApiClient.connect(
    'https://example.com',
    'test-token-0123456789abcdefghij0123456789',
    getRetryDelays: const [],
    httpClient: MockClient((_) async => http.Response('{}', 404)),
  );
  return ChatController(_chat, client, AppState(client), draftStore: store);
}

void main() {
  group('queue while running', () {
    test(
      'send queues while a reply runs and the queue pumps when idle',
      () async {
        final chat = _chatFor();
        chat.runningTaskId = 'task-1';

        expect(await chat.send('one after the other'), isTrue);
        expect(chat.queued, hasLength(1));
        expect(chat.queued.single.text, 'one after the other');
        expect(chat.draft, isEmpty);

        chat.runningTaskId = null;
        chat.removeQueued(0);
        expect(chat.queued, isEmpty);
      },
    );

    test(
      'queueSend refuses a blank message, an over-long one and a full queue',
      () async {
        final chat = _chatFor();
        expect(await chat.queueSend('   '), isFalse);
        expect(await chat.queueSend('x' * (maxMessageChars + 1)), isFalse);
        for (var i = 0; i < maxQueuedMessages; i++) {
          expect(await chat.queueSend('msg $i'), isTrue);
        }
        expect(await chat.queueSend('one too many'), isFalse);
        expect(chat.queued, hasLength(maxQueuedMessages));
      },
    );
  });

  group('picker queries', () {
    test('mention query needs @ at the start or after whitespace', () {
      expect(mentionQuery('ask @analyst about this @maker'), 'maker');
      expect(mentionQuery('ping @Analyst'), 'Analyst');
      expect(mentionQuery('email me at a@b'), isNull);
      expect(mentionQuery('@'), '');
      expect(mentionQuery('hi @'), '');
      expect(mentionQuery('@ana lyst'), isNull);
    });

    test('slash query needs a bare / on the last line', () {
      expect(slashQuery('/att'), 'att');
      expect(slashQuery('hello\n/st'), 'st');
      expect(slashQuery('/attach file'), isNull);
      expect(slashQuery('not a command'), isNull);
    });

    test('slash actions filter by their command', () {
      expect(mobileSlashMatches('').map((a) => a.command), ['attach', 'stop']);
      expect(mobileSlashMatches('sto').map((a) => a.command), ['stop']);
      expect(mobileSlashMatches('xyz'), isEmpty);
    });
  });

  group('draft store', () {
    test('drafts round-trip per chat and drop when emptied', () async {
      final dir = await Directory.systemTemp.createTemp('serabot_drafts_test');
      addTearDown(() => dir.delete(recursive: true));
      final store = DraftStore(directory: () async => dir);

      await store.saveAll({_chat: 'half-typed', 'dir:other': 'keep me'});
      expect((await store.loadAll())[_chat], 'half-typed');

      final chat = _chatFor(store: store);
      await chat.restoreDraft();
      expect(chat.draft, 'half-typed');

      chat.noteDraft('');
      await Future<void>.delayed(const Duration(milliseconds: 700));
      expect((await store.loadAll()).containsKey(_chat), isFalse);
      expect((await store.loadAll())['dir:other'], 'keep me');
    });

    test('a broken file reads as no drafts', () async {
      final dir = await Directory.systemTemp.createTemp('serabot_drafts_bad');
      addTearDown(() => dir.delete(recursive: true));
      await File('${dir.path}/serabot_drafts.json')
          .writeAsString('this is not json');
      final store = DraftStore(directory: () async => dir);
      expect(await store.loadAll(), isEmpty);
    });
  });
}
