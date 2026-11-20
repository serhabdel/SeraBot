import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/usage_insights_models.dart';

void main() {
  test('parses the documented insights shape', () {
    final insights = UsageInsights.fromJson({
      'days': 7,
      'scope': 'me',
      'channels': [
        {
          'channel': 'mobile',
          'turns': 3,
          'tokens': 1200,
          'last_at': '2026-11-19T10:00:00Z',
        },
        {'channel': 'automation', 'turns': 1, 'tokens': 0, 'last_at': null},
      ],
      'tunnel': {
        'tunnels': 2,
        'bytes_to_provider': 1024,
        'bytes_from_provider': 3 * 1024 * 1024,
        'per_day': [
          {
            'day': '2026-11-19',
            'tunnels': 2,
            'bytes_to_provider': 1024,
            'bytes_from_provider': 2048,
          },
        ],
        'per_host': [
          {
            'host': 'api.example.test',
            'tunnels': 2,
            'bytes': 4096,
            'avg_seconds': 3,
          },
        ],
        'relayed_calls': 1,
        'total_calls': 4,
      },
      'relay_online': true,
      'models': [
        {
          'provider': 'acme',
          'model': 'model-small',
          'calls': 4,
          'avg_latency_ms': 850,
          'p95_latency_ms': 2400,
          'relayed_calls': 1,
          'failovers': 2,
        },
      ],
      'resources': null,
    });

    expect(insights.days, 7);
    expect(insights.replies, 4);
    expect(insights.tokens, 1200);
    expect(insights.channels.first.channel, 'mobile');
    expect(insights.channels.last.lastAt, isNull);
    expect(insights.relayOnline, isTrue);
    expect(insights.tunnel.relayedPercent, 25);
    expect(insights.tunnel.bytes, 1024 + 3 * 1024 * 1024);
    expect(insights.tunnel.perDay.single.bytes, 3072);
    expect(insights.tunnel.perHost.single.avgSeconds, 3);
    expect(insights.models.single.relayedPercent, 25);
    expect(insights.models.single.failovers, 2);
    expect(insights.resources, isNull, reason: 'members get no server load');
  });

  test('missing and wrong-typed fields fall back to defaults', () {
    final insights = UsageInsights.fromJson({
      'channels': [
        {'channel': 'telegram', 'turns': '5', 'tokens': 'many'},
        'not a row',
      ],
      'tunnel': 'nonsense',
      'models': [
        {'model': 'model-small'},
      ],
      'relay_online': 'yes',
    });

    expect(insights.days, 7);
    expect(insights.channels.single.turns, 5);
    expect(insights.channels.single.tokens, 0);
    expect(insights.tunnel.tunnels, 0);
    expect(insights.tunnel.perDay, isEmpty);
    expect(insights.models.single.provider, '');
    expect(insights.models.single.calls, 0);
    expect(insights.relayOnline, isFalse);
  });

  test('admin resource samples are parsed and clamped', () {
    final insights = UsageInsights.fromJson({
      'days': 1,
      'resources': [
        {
          'at': '2026-11-20T09:00:00Z',
          'cpu_percent': 140.5,
          'memory_percent': 42,
          'process_rss_bytes': 104857600,
        },
      ],
    });

    final sample = insights.resources!.single;
    expect(sample.cpuPercent, 100.0);
    expect(sample.memoryPercent, 42.0);
    expect(sample.processRssBytes, 104857600);
    expect(insights.resources, isNotNull);
  });

  test('empty resource list is still an admin view', () {
    final insights = UsageInsights.fromJson({'resources': <Object>[]});
    expect(insights.resources, isEmpty);
  });

  test('channel labels follow the desktop card', () {
    expect(channelLabel('desktop'), 'This PC app');
    expect(channelLabel('mobile'), 'Phone app');
    expect(channelLabel('telegram'), 'Telegram');
    expect(channelLabel('whatsapp'), 'WhatsApp');
    expect(channelLabel('app'), 'App (before 0.67)');
    expect(channelLabel('api'), 'API and scripts');
    expect(channelLabel('automation'), 'Schedules and watches');
    expect(channelLabel('something-new'), 'Schedules and watches');
  });

  test('shares, sizes, latency and counts read well', () {
    expect(sharePercent(1, 3), 33);
    expect(sharePercent(5, 0), 0);
    expect(sharePercent(9, 4), 100);
    expect(humanBytes(512), '512 B');
    expect(humanBytes(2048), '2 KB');
    expect(humanBytes(3 * 1024 * 1024), '3.0 MB');
    expect(humanBytes(2 * 1024 * 1024 * 1024), '2.0 GB');
    expect(humanLatency(0), '—');
    expect(humanLatency(850), '850 ms');
    expect(humanLatency(2400), '2.4 s');
    expect(formatCount(0), '0');
    expect(formatCount(999), '999');
    expect(formatCount(1234567), '1,234,567');
  });

  test('text from the server is cleaned and bounded', () {
    expect(insightText('ab\u0007c'), 'abc');
    expect(insightText('  spaced  '), 'spaced');
    expect(insightText(42), '');
    expect(insightText('abcdef', 3), 'abc');
    expect(insightOptText('   '), isNull);
  });
}
