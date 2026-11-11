import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/workflows_models.dart';

void main() {
  test('parses documented routine shape', () {
    final rows = parseRoutines({
      'routines': [
        {
          'name': 'morning_brief',
          'enabled': true,
          'next_run_at': '2026-11-10T06:30:00Z',
          'last_run_for': '2026-11-09',
          'config': {'time': '07:30', 'timezone': 'Europe/Rome'},
        },
      ],
    });
    expect(rows, isNotNull);
    expect(rows!.single.time, '07:30');
    expect(rows.single.timezone, 'Europe/Rome');
  });

  test('parses monitoring and active shapes', () {
    final sources = parseSources({
      'sources': [
        {
          'name': 'portal',
          'kind': 'rss',
          'enabled': true,
          'url': 'https://example.test/feed',
          'last_run': {
            'ok': false,
            'error': 'timeout',
            'items_seen': 0,
            'started_at': '2026-11-09T10:00:00Z',
          },
        },
      ],
    });
    expect(sources!.single.lastRun!.ok, isFalse);

    final active = parseActiveWorkflows({
      'workflows': [
        {
          'conversation_id': 'dir:main',
          'goal': 'Ship',
          'done': 1,
          'total': 3,
          'updated_at': '2026-11-09T12:00:00Z',
        },
      ],
    });
    expect(active!.single.done, 1);
    expect(planProgress(1, 3), '1/3');
  });

  test('parses redacted trace rows', () {
    final page = parseTraces({
      'conversation_id': 'dir:trace-a',
      'traces': [
        {
          'id': 1,
          'at': '2026-11-09T10:00:00Z',
          'event': 'tool.completed',
          'tool': 'browser_fill',
          'canonical_hash': 'hash-synthetic',
          'destination': 'https://bank.example',
          'elapsed_ms': 42,
        },
      ],
    });
    expect(page.traces.single.elapsedMs, 42);
    expect(page.conversationId, 'dir:trace-a');
  });

  group('display helpers', () {
    final now = DateTime.utc(2026, 11, 10, 12);

    test('relative time reads naturally', () {
      expect(relativeTime('2026-11-10T11:55:00Z', now: now), '5m ago');
      expect(relativeTime('2026-11-10T09:00:00Z', now: now), '3h ago');
      expect(relativeTime('2026-11-08T12:00:00Z', now: now), '2d ago');
      expect(relativeTime('2026-11-10T14:00:00Z', now: now), 'in 2h');
      expect(relativeTime('2026-11-10T12:00:20Z', now: now), 'just now');
    });

    test('an unreadable stamp is shown, not hidden', () {
      expect(relativeTime('garbage', now: now), 'garbage');
      expect(relativeTime('', now: now), '\u2014');
    });

    test('source host and short ids', () {
      expect(sourceHost('https://example.test/feed?k=1'), 'example.test');
      expect(sourceHost('not a url'), isNull);
      expect(shortId('abc'), 'abc');
      expect(shortId('0123456789abcdefghij'), '0123456789abc\u2026');
    });

    test('plan label prefers the conversation name', () {
      expect(planConversationLabel('  Trip  ', 'x'), 'Trip');
      expect(planConversationLabel(' ', 'dir:main'), 'dir:main');
      expect(planConversationLabel(null, '0123456789abcdefghij'),
          '0123456789abc\u2026');
    });
  });
}
