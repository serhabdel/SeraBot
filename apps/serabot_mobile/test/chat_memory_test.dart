import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/chat_controller.dart';
import 'package:serabot_mobile/chat_models.dart';

final _token = 'k' * 43;
const _chat = 'dir:synthetic';

void main() {
  test('a live turn.memory event fills its turn, keyed by task', () async {
    final client = ApiClient.connect(
      'https://example.com',
      _token,
      getRetryDelays: const [],
      httpClient: MockClient((_) async => http.Response('{}', 404)),
    );
    final app = AppState(client);
    final chat = ChatController(_chat, client, app);

    app.handleEvent(
      const ServerEvent(1, 'turn.memory', {
        'task_id': 'task_live',
        'conversation_id': _chat,
        'recalled': [
          {
            'id': 'm1',
            'kind': 'preference',
            'origin': 'explicit',
            'excerpt': 'Likes green tea',
          },
        ],
        'recalled_total': 1,
      }),
    );
    app.handleEvent(
      const ServerEvent(2, 'turn.memory', {
        'task_id': 'task_other',
        'conversation_id': 'dir:elsewhere',
        'recalled': [
          {'id': 'm2', 'excerpt': 'Belongs to another chat'},
        ],
        'recalled_total': 1,
      }),
    );
    await Future<void>.delayed(Duration.zero);

    expect(
      chat.memoryFor('task_live').recalled.single.excerpt,
      'Likes green tea',
    );
    expect(
      chat.memoryFor('task_other').isEmpty,
      isTrue,
      reason: 'other chats are ignored',
    );
    expect(chat.memoryFor(null).isEmpty, isTrue);

    chat.dispose();
    app.dispose();
    client.close();
  });

  test(
    'reopening a chat replays the stored memory of the last reply',
    () async {
      final client = ApiClient.connect(
        'https://example.com',
        _token,
        getRetryDelays: const [],
        httpClient: MockClient((request) async {
          expect(request.url.path, '/v1/conversations/$_chat/activity');
          return http.Response(
            jsonEncode({
              'conversation_id': _chat,
              'steps': [
                {
                  'id': 1,
                  'at': '2026-11-20T09:00:00Z',
                  'kind': 'turn.step',
                  'payload': {},
                },
                {
                  'id': 2,
                  'at': '2026-11-20T09:00:01Z',
                  'kind': 'turn.memory',
                  'payload': {
                    'task_id': 'task_old',
                    'conversation_id': _chat,
                    'saved': [
                      {
                        'id': 'm3',
                        'kind': 'fact',
                        'origin': 'extracted',
                        'excerpt': 'Works on Tuesdays',
                      },
                    ],
                    'saved_total': 1,
                  },
                },
              ],
            }),
            200,
            headers: {'content-type': 'application/json'},
          );
        }),
      );
      final app = AppState(client);
      final chat = ChatController(_chat, client, app);

      await chat.loadMemory();

      expect(
        chat.memoryFor('task_old').saved.single.excerpt,
        'Works on Tuesdays',
      );
      expect(
        chat.currentMemory.saved.single.excerpt,
        'Works on Tuesdays',
        reason: 'the last reply is the one shown when nothing is running',
      );
      expect(chat.currentMemory.summary, 'Saved 1 memory');

      chat.dispose();
      app.dispose();
      client.close();
    },
  );
}
