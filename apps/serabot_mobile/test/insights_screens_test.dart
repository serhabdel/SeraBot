import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/bot_library_models.dart';
import 'package:serabot_mobile/screens/bot_library_screen.dart';
import 'package:serabot_mobile/screens/usage_insights_screen.dart';

final _token = 'w' * 43;

http.Response _json(Object body) => http.Response(
  jsonEncode(body),
  200,
  headers: {'content-type': 'application/json'},
);

AppState _app(Future<http.Response> Function(http.Request) handler) {
  final client = ApiClient.connect(
    'https://example.com',
    _token,
    getRetryDelays: const [],
    httpClient: MockClient(handler),
  );
  return AppState(client);
}

final Map<String, dynamic> _insightsBody = {
  'days': 7,
  'scope': 'me',
  'channels': [
    {'channel': 'mobile', 'turns': 3, 'tokens': 900},
    {'channel': 'automation', 'turns': 1, 'tokens': 100},
  ],
  'tunnel': {
    'tunnels': 1,
    'bytes_to_provider': 1024,
    'bytes_from_provider': 2048,
    'per_day': [
      {
        'day': '2026-11-19',
        'tunnels': 1,
        'bytes_to_provider': 1024,
        'bytes_from_provider': 2048,
      },
    ],
    'per_host': [
      {
        'host': 'models.example.test',
        'tunnels': 1,
        'bytes': 3072,
        'avg_seconds': 2,
      },
    ],
    'relayed_calls': 1,
    'total_calls': 4,
  },
  'relay_online': true,
  'models': [
    {
      'provider': 'acme',
      'model': 'synthetic-model',
      'calls': 4,
      'avg_latency_ms': 850,
      'p95_latency_ms': 2400,
      'relayed_calls': 1,
      'failovers': 0,
    },
  ],
  'resources': [
    {
      'at': '2026-11-20T09:00:00Z',
      'cpu_percent': 12.5,
      'memory_percent': 40,
      'process_rss_bytes': 1000,
    },
    {
      'at': '2026-11-20T09:05:00Z',
      'cpu_percent': 20,
      'memory_percent': 41,
      'process_rss_bytes': 1100,
    },
  ],
};

final Map<String, dynamic> _usageBody = {
  'totals': {
    'calls': 4,
    'prompt_tokens': 700,
    'completion_tokens': 300,
    'total_tokens': 1000,
  },
  'per_model': <Object>[],
  'tools': [
    {'tool': 'synthetic_tool', 'completed': 2, 'failed': 1},
  ],
};

/// The screen's one scrollable list; cards below the fold are built lazily.
Finder _listScroll() => find.descendant(
  of: find.byType(ListView),
  matching: find.byType(Scrollable),
);

Future<void> _reveal(WidgetTester tester, Finder finder) {
  return tester.scrollUntilVisible(finder, 300, scrollable: _listScroll());
}

void main() {
  testWidgets('usage insights renders every card for an admin', (tester) async {
    final app = _app((request) async {
      if (request.url.path == '/v1/usage/insights') {
        return _json(_insightsBody);
      }
      if (request.url.path == '/v1/usage') {
        return _json(_usageBody);
      }
      return http.Response('', 404);
    });
    await tester.pumpWidget(MaterialApp(home: UsageInsightsScreen(app: app)));
    await tester.pumpAndSettle();

    expect(find.text('Replies'), findsOneWidget);
    expect(find.text('4'), findsOneWidget, reason: 'replies across channels');
    expect(find.text('1,000'), findsOneWidget, reason: 'tokens from /v1/usage');
    expect(
      find.text('3'),
      findsOneWidget,
      reason: 'tool runs, completed plus failed',
    );
    expect(find.text('Phone app'), findsOneWidget);
    expect(find.text('Schedules and watches'), findsOneWidget);
    expect(find.text('Relaying'), findsOneWidget);
    expect(find.text('models.example.test'), findsOneWidget);
    await _reveal(tester, find.textContaining('synthetic-model'));
    expect(find.textContaining('synthetic-model'), findsOneWidget);
    await _reveal(tester, find.text('Server load'));
    expect(find.text('Server load'), findsOneWidget);
    expect(find.text('CPU'), findsOneWidget);
    expect(find.text('Memory'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('usage insights hides server load for a member', (tester) async {
    final member = Map<String, dynamic>.from(_insightsBody)
      ..remove('resources');
    final app = _app((request) async {
      if (request.url.path == '/v1/usage/insights') {
        return _json(member);
      }
      return _json(_usageBody);
    });
    await tester.pumpWidget(MaterialApp(home: UsageInsightsScreen(app: app)));
    await tester.pumpAndSettle();

    await _reveal(tester, find.text('Model speed and reliability'));
    expect(find.text('Server load'), findsNothing);
    expect(find.text('PC tunnel'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('bot library groups items and opens the details sheet', (
    tester,
  ) async {
    final today = todayUtcDay();
    final app = _app((request) async {
      expect(request.url.path, '/v1/bots/synthetic-bot/library');
      return _json({
        'bot': 'synthetic-bot',
        'items': [
          {
            'kind': 'generated',
            'id': 'artifact_today',
            'tool': 'make_report',
            'name': 'Synthetic report',
            'filename': 'report.pdf',
            'mime': 'application/pdf',
            'size_bytes': null,
            'conversation_id': 'dir:synthetic',
            'conversation_title': 'Synthetic chat',
            'created_at': '${today}T08:00:00Z',
          },
          {
            'kind': 'uploaded',
            'id': 'att_old',
            'tool': 'upload',
            'filename': 'notes.txt',
            'mime': 'text/plain',
            'size_bytes': 2048,
            'conversation_id': 'dir:synthetic',
            'conversation_title': null,
            'created_at': '2026-01-02T10:00:00Z',
          },
        ],
      });
    });
    await tester.pumpWidget(
      MaterialApp(
        home: BotLibraryScreen(
          app: app,
          botId: 'synthetic-bot',
          botName: 'Synthetic bot',
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('Today'), findsOneWidget);
    expect(find.text('Earlier'), findsOneWidget);
    expect(find.text('Synthetic report'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('library-artifact_today')));
    await tester.pumpAndSettle();
    expect(
      find.text('Not recorded'),
      findsOneWidget,
      reason: 'no size for a made file',
    );
    expect(find.text('Made by a bot'), findsWidgets);

    await tester.tap(find.byType(ModalBarrier).last, warnIfMissed: false);
    await tester.pumpAndSettle();

    await tester.tap(find.byType(ChoiceChip).at(2));
    await tester.pumpAndSettle();
    expect(
      find.text('Synthetic report'),
      findsNothing,
      reason: 'Uploaded filter hides it',
    );
    expect(find.text('notes.txt'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}
