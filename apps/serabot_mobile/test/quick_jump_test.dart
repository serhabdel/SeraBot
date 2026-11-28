import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/auth.dart';
import 'package:serabot_mobile/device.dart';
import 'package:serabot_mobile/models.dart';
import 'package:serabot_mobile/quick_jump.dart';
import 'package:serabot_mobile/screens/chats_screen.dart';
import 'package:serabot_mobile/screens/settings_screen.dart';
import 'package:serabot_mobile/screens/tasks_screen.dart';
import 'package:serabot_mobile/session.dart';
import 'package:serabot_mobile/theme.dart';

class _MemStore implements TokenStore {
  @override
  Future<({String endpoint, String token})?> read() async => null;
  @override
  Future<void> write(String endpoint, String token) async {}
  @override
  Future<void> clear() async {}
}

class _NoopDevice implements DeviceActions {
  @override
  Future<List<PickedFile>> pickFiles({bool imagesOnly = false}) async =>
      const [];
  @override
  Future<PickedFile?> pickAnyFile({int maxBytes = 25 * 1024 * 1024}) async =>
      null;
  @override
  Future<bool> startRecording() async => false;
  @override
  Future<RecordedAudio?> stopRecording() async => null;
  @override
  Future<void> cancelRecording() async {}
  @override
  Future<void> shareFile(String filename, List<int> bytes) async {}
}

http.Response _json(Object body, [int status = 200]) =>
    http.Response(jsonEncode(body), status,
        headers: {'content-type': 'application/json'});

void main() {
  group('fuzzyMatchScore', () {
    test('empty or whitespace query returns 0', () {
      expect(fuzzyMatchScore('', 'Chats'), 0);
      expect(fuzzyMatchScore('   ', 'Settings'), 0);
    });

    test('subsequence matches in order and rejects out-of-order', () {
      expect(fuzzyMatchScore('son5', 'claude-sonnet-5'), isNotNull);
      expect(fuzzyMatchScore('c5s', 'claude-sonnet-5'), isNull);
      expect(fuzzyMatchScore('xyz', 'claude-sonnet-5'), isNull);
    });

    test('contiguous substring outscores scattered subsequence', () {
      final exact = fuzzyMatchScore('sonnet', 'claude-sonnet-5')!;
      final scattered = fuzzyMatchScore('sonnet', 's-o-n-n-e-t-x')!;
      expect(exact, greaterThan(scattered));
    });

    test('multi-term requires all terms to match', () {
      expect(fuzzyMatchScore('daily plan', 'Daily project plan'), isNotNull);
      expect(fuzzyMatchScore('daily missing', 'Daily project plan'), isNull);
    });
  });

  group('fuzzyMatchRanges', () {
    test('returns empty for empty query or non-matching text', () {
      expect(fuzzyMatchRanges('', 'Chats'), isEmpty);
      expect(fuzzyMatchRanges('zzz', 'Chats'), isEmpty);
    });

    test('returns single contiguous range for substring match', () {
      expect(
        fuzzyMatchRanges('sonnet', 'claude-sonnet-5'),
        [(start: 7, end: 13)],
      );
    });

    test('returns sorted merged ranges for subsequence and multi-term', () {
      expect(
        fuzzyMatchRanges('cs5', 'claude-sonnet-5'),
        [(start: 0, end: 1), (start: 7, end: 8), (start: 14, end: 15)],
      );
      expect(
        fuzzyMatchRanges('daily plan', 'Daily project plan'),
        [(start: 0, end: 5), (start: 14, end: 18)],
      );
    });
  });

  group('CalendarProviderWarning & CalendarEventsResult', () {
    test('parses events and provider_warnings', () {
      final res = CalendarEventsResult.fromJson({
        'events': [
          {
            'id': 'ev1',
            'title': 'Standup',
            'start': '2026-11-28T09:00:00Z',
            'provider': 'google_calendar',
          }
        ],
        'provider_warnings': [
          {
            'provider': 'outlook_calendar',
            'status_code': 401,
            'message': 'Outlook Calendar token expired',
            'hint': 'Reconnect Outlook in desktop Connectors.',
          }
        ],
      });
      expect(res.events, hasLength(1));
      expect(res.events.first.title, 'Standup');
      expect(res.warnings, hasLength(1));
      expect(res.warnings.first.provider, 'outlook_calendar');
      expect(res.warnings.first.statusCode, 401);
      expect(res.warnings.first.message, 'Outlook Calendar token expired');
      expect(
        res.warnings.first.hint,
        'Reconnect Outlook in desktop Connectors.',
      );
    });

    test('handles absent warnings and blank hint', () {
      final res = CalendarEventsResult.fromJson({
        'events': [],
        'provider_warnings': [
          {
            'provider': 'google_calendar',
            'message': 'Temporary timeout',
            'hint': '   ',
          }
        ],
      });
      expect(res.events, isEmpty);
      expect(res.warnings, hasLength(1));
      expect(res.warnings.first.hint, isNull);
    });

    test('calendarEventsWithWarnings and calendarEvents delegate cleanly',
        () async {
      final httpClient = MockClient((req) async {
        expect(req.url.path, '/v1/tasks/calendar');
        return _json({
          'events': [
            {'id': 'e1', 'title': 'Sync', 'start': '2026-11-28T10:00:00Z'}
          ],
          'provider_warnings': [
            {
              'provider': 'outlook_calendar',
              'status_code': 503,
              'message': 'Outlook unavailable',
              'hint': 'Try again shortly.',
            }
          ],
        });
      });

      final client = ApiClient.connect(
        'https://example.com',
        't' * 32,
        httpClient: httpClient,
      );
      addTearDown(client.close);

      final result = await client.calendarEventsWithWarnings();
      expect(result.events, hasLength(1));
      expect(result.warnings, hasLength(1));
      expect(result.warnings.first.provider, 'outlook_calendar');

      final eventsOnly = await client.calendarEvents();
      expect(eventsOnly, hasLength(1));
      expect(eventsOnly.first.id, 'e1');
    });
  });

  group('AI Provider Quotas parsing', () {
    test('parses provider accounts and quota windows', () {
      final p = AiProviderSummary.fromJson({
        'id': 'antigravity',
        'kind': 'google-antigravity',
        'auth': 'oauth',
        'status': 'ok',
        'health': 'healthy',
        'accounts': [
          {
            'id': 'acc1',
            'label': 'Personal',
            'auth_kind': 'oauth',
            'status': 'ok',
            'identity': {'email': 'a***@example.com', 'plan': 'pro'},
            'quota': {
              'sampled_at': '2026-11-28T01:00:00Z',
              'source': 'summary',
              'state': 'ok',
              'windows': [
                {
                  'scope': 'subscription',
                  'label': '5h',
                  'used_percent': 37.5,
                  'remaining': 0.625,
                  'resets_at': '2026-11-28T05:00:00Z',
                }
              ],
            },
          },
          {
            'id': 'acc2',
            'label': 'Backup',
            'auth_kind': 'api_key',
            'status': 'ok',
            'identity': {},
            'quota': null,
          }
        ],
      });
      expect(p.accounts, hasLength(2));
      expect(p.accounts[0].email, 'a***@example.com');
      expect(p.accounts[0].plan, 'pro');
      expect(p.accounts[0].quota?.windows.first.remainingPercent, 63);
      expect(p.accounts[1].quota, isNull);
    });
  });

  group('Widget coverage', () {
    testWidgets('QuickJumpSheet opens from ChatsScreen and selects tab',
        (tester) async {
      final httpClient = MockClient((req) async {
        final key = '${req.method} ${req.url.path}';
        if (key == 'GET /v1/conversations') {
          return _json({
            'conversations': [
              {
                'id': 'dir:main',
                'kind': 'direct',
                'title': 'Daily plan',
                'created_at': 'a',
                'updated_at': '2026-11-28T01:00:00Z',
                'unread_count': 0,
                'accepts_turns': true,
              }
            ]
          });
        }
        if (key == 'GET /v1/bots') {
          return _json({
            'bots': [
              {
                'id': 'main',
                'role': 'orchestrator',
                'enabled': true,
                'live': true,
                'display_name': 'Sera',
                'tools': [],
                'skills': [],
              }
            ]
          });
        }
        if (key == 'GET /v1/approvals') {
          return _json({'approvals': []});
        }
        return _json({});
      });

      final api = ApiClient.connect('https://example.com', 'k' * 32,
          httpClient: httpClient);
      final app = AppState(api)..start(events: false);
      addTearDown(app.dispose);
      int? jumpedTab;

      await tester.pumpWidget(MaterialApp(
        home: ChatsScreen(
          app: app,
          device: _NoopDevice(),
          onSelectTab: (i) => jumpedTab = i,
        ),
      ));
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('quick-jump-button')));
      await tester.pumpAndSettle();

      expect(find.byKey(const Key('quick-jump-field')), findsOneWidget);
      await tester.enterText(
          find.byKey(const Key('quick-jump-field')), 'tasks');
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('quick-jump-tab-1')));
      await tester.pumpAndSettle();
      expect(jumpedTab, 1);
    });

    testWidgets('TasksScreen Calendar view renders provider warning card',
        (tester) async {
      final httpClient = MockClient((req) async {
        final key = '${req.method} ${req.url.path}';
        if (key == 'GET /v1/tasks') {
          return _json({
            'tasks': [],
            'counts': {'todo': 0, 'in_progress': 0, 'done': 0},
            'sync': {'provider': 'none', 'health': 'off'},
          });
        }
        if (key == 'GET /v1/tasks/calendar') {
          return _json({
            'events': [],
            'provider_warnings': [
              {
                'provider': 'google_calendar',
                'status_code': 401,
                'message': 'Google Calendar sign-in expired',
                'hint': 'Reconnect Google Calendar on desktop.',
              }
            ],
          });
        }
        return _json({});
      });

      final api = ApiClient.connect('https://example.com', 'k' * 32,
          httpClient: httpClient);
      final app = AppState(api);
      addTearDown(app.dispose);

      await tester.pumpWidget(MaterialApp(home: TasksScreen(app: app)));
      await tester.pumpAndSettle();

      await tester.tap(find.text('Calendar'));
      await tester.pumpAndSettle();

      expect(
        find.byKey(const Key('calendar-warning-google_calendar')),
        findsOneWidget,
      );
      expect(find.text('Google Calendar sign-in expired'), findsOneWidget);
      expect(
        find.text('Reconnect Google Calendar on desktop.'),
        findsOneWidget,
      );
    });

    testWidgets('SettingsScreen renders AI Providers & Quotas', (tester) async {
      final httpClient = MockClient((req) async {
        final key = '${req.method} ${req.url.path}';
        if (key == 'GET /v1/connectors') {
          return _json({'connectors': []});
        }
        if (key == 'GET /v1/providers') {
          return _json({
            'providers': [
              {
                'id': 'muse',
                'kind': 'muse-code',
                'auth': 'oauth',
                'status': 'ok',
                'health': 'healthy',
                'accounts': [
                  {
                    'id': 'a1',
                    'label': 'Main',
                    'auth_kind': 'oauth',
                    'status': 'ok',
                    'identity': {'email': 'u***@example.com'},
                    'quota': {
                      'state': 'ok',
                      'windows': [
                        {
                          'scope': 'subscription',
                          'label': '5h',
                          'used_percent': 25,
                          'remaining': 0.75,
                        }
                      ],
                    },
                  }
                ],
              }
            ],
          });
        }
        if (key == 'GET /v1/usage') {
          return _json({
            'days': 30,
            'totals': {
              'calls': 1,
              'input_tokens': 10,
              'output_tokens': 5,
              'total_tokens': 15
            },
            'per_model': [],
          });
        }
        return _json({});
      });

      final session = OwnerSession(
        store: _MemStore(),
        authApi: AuthApi(httpClient: httpClient),
        httpClient: httpClient,
      );
      final api = ApiClient.connect('https://example.com', 'k' * 32,
          httpClient: httpClient);
      final app = AppState(api);
      addTearDown(app.dispose);
      final theme = ThemeController();

      await tester.pumpWidget(MaterialApp(
        home: SettingsScreen(
          session: session,
          app: app,
          theme: theme,
          versionLoader: () async => '0.74.32',
        ),
      ));
      await tester.pumpAndSettle();

      expect(find.text('AI Providers & Quotas'), findsOneWidget);
      expect(find.text('muse \u00b7 Main'), findsOneWidget);
      expect(find.text('75% left'), findsOneWidget);
    });
  });
}
