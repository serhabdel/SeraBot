// Usage insights (0.67): parsing and view rules for GET /v1/usage/insights.
// Mirrors crates/serabot-desktop/src/usage_insights.rs. Counts and sizes only,
// never message text. Every field is optional on the wire: a missing or
// wrong-typed value falls back to its default, so the screen never breaks on
// an older or newer server.

const _maxRows = 200;
const _maxText = 200;

/// A whole number from any JSON number or numeric string; 0 otherwise.
int insightInt(Object? v) {
  if (v is int) {
    return v;
  }
  if (v is num && v.isFinite) {
    return v.toInt();
  }
  if (v is String) {
    return int.tryParse(v.trim()) ?? 0;
  }
  return 0;
}

/// A decimal from any finite JSON number; 0 otherwise.
double insightDouble(Object? v) {
  if (v is num && v.isFinite) {
    return v.toDouble();
  }
  return 0;
}

/// Display text: control characters removed, at most [max] code points.
String insightText(Object? v, [int max = _maxText]) {
  if (v is! String) {
    return '';
  }
  final kept = v.runes
      .where((r) => r > 0x1f && !(r >= 0x7f && r <= 0x9f))
      .take(max);
  return String.fromCharCodes(kept).trim();
}

/// Same as [insightText] but null when empty.
String? insightOptText(Object? v, [int max = _maxText]) {
  final text = insightText(v, max);
  return text.isEmpty ? null : text;
}

/// Object rows of a JSON list, bounded. Anything else is an empty list.
List<Map<String, dynamic>> insightRows(Object? v) {
  if (v is! List) {
    return const [];
  }
  return v.whereType<Map<String, dynamic>>().take(_maxRows).toList();
}

Map<String, dynamic> _asMap(Object? v) =>
    v is Map<String, dynamic> ? v : const <String, dynamic>{};

class InsightChannel {
  const InsightChannel({
    required this.channel,
    this.turns = 0,
    this.tokens = 0,
    this.lastAt,
  });

  factory InsightChannel.fromJson(Map<String, dynamic> json) {
    return InsightChannel(
      channel: insightText(json['channel'], 32),
      turns: insightInt(json['turns']),
      tokens: insightInt(json['tokens']),
      lastAt: insightOptText(json['last_at'], 64),
    );
  }

  final String channel;
  final int turns;
  final int tokens;
  final String? lastAt;
}

class InsightTunnelDay {
  const InsightTunnelDay({
    required this.day,
    this.tunnels = 0,
    this.bytesToProvider = 0,
    this.bytesFromProvider = 0,
  });

  factory InsightTunnelDay.fromJson(Map<String, dynamic> json) {
    return InsightTunnelDay(
      day: insightText(json['day'], 16),
      tunnels: insightInt(json['tunnels']),
      bytesToProvider: insightInt(json['bytes_to_provider']),
      bytesFromProvider: insightInt(json['bytes_from_provider']),
    );
  }

  final String day;
  final int tunnels;
  final int bytesToProvider;
  final int bytesFromProvider;

  int get bytes => bytesToProvider + bytesFromProvider;
}

class InsightTunnelHost {
  const InsightTunnelHost({
    required this.host,
    this.tunnels = 0,
    this.bytes = 0,
    this.avgSeconds = 0,
  });

  factory InsightTunnelHost.fromJson(Map<String, dynamic> json) {
    return InsightTunnelHost(
      host: insightText(json['host'], 253),
      tunnels: insightInt(json['tunnels']),
      bytes: insightInt(json['bytes']),
      avgSeconds: insightInt(json['avg_seconds']),
    );
  }

  final String host;
  final int tunnels;
  final int bytes;
  final int avgSeconds;
}

class InsightTunnel {
  const InsightTunnel({
    this.tunnels = 0,
    this.bytesToProvider = 0,
    this.bytesFromProvider = 0,
    this.perDay = const [],
    this.perHost = const [],
    this.relayedCalls = 0,
    this.totalCalls = 0,
  });

  factory InsightTunnel.fromJson(Object? value) {
    final json = _asMap(value);
    return InsightTunnel(
      tunnels: insightInt(json['tunnels']),
      bytesToProvider: insightInt(json['bytes_to_provider']),
      bytesFromProvider: insightInt(json['bytes_from_provider']),
      perDay: [
        for (final row in insightRows(json['per_day']))
          InsightTunnelDay.fromJson(row),
      ],
      perHost: [
        for (final row in insightRows(json['per_host']))
          InsightTunnelHost.fromJson(row),
      ],
      relayedCalls: insightInt(json['relayed_calls']),
      totalCalls: insightInt(json['total_calls']),
    );
  }

  final int tunnels;
  final int bytesToProvider;
  final int bytesFromProvider;
  final List<InsightTunnelDay> perDay;
  final List<InsightTunnelHost> perHost;
  final int relayedCalls;
  final int totalCalls;

  int get bytes => bytesToProvider + bytesFromProvider;

  /// Share of model calls that went through the PC, 0 to 100.
  int get relayedPercent => sharePercent(relayedCalls, totalCalls);
}

class InsightModel {
  const InsightModel({
    required this.provider,
    required this.model,
    this.calls = 0,
    this.avgLatencyMs = 0,
    this.p95LatencyMs = 0,
    this.relayedCalls = 0,
    this.failovers = 0,
  });

  factory InsightModel.fromJson(Map<String, dynamic> json) {
    return InsightModel(
      provider: insightText(json['provider'], 64),
      model: insightText(json['model'], 128),
      calls: insightInt(json['calls']),
      avgLatencyMs: insightInt(json['avg_latency_ms']),
      p95LatencyMs: insightInt(json['p95_latency_ms']),
      relayedCalls: insightInt(json['relayed_calls']),
      failovers: insightInt(json['failovers']),
    );
  }

  final String provider;
  final String model;
  final int calls;
  final int avgLatencyMs;
  final int p95LatencyMs;
  final int relayedCalls;
  final int failovers;

  /// Share of this model's calls sent through the PC, 0 to 100.
  int get relayedPercent => sharePercent(relayedCalls, calls);
}

/// One server load sample (admin only). Percentages are 0 to 100.
class InsightResourceSample {
  const InsightResourceSample({
    this.at = '',
    this.cpuPercent = 0,
    this.memoryPercent = 0,
    this.processRssBytes = 0,
  });

  factory InsightResourceSample.fromJson(Map<String, dynamic> json) {
    return InsightResourceSample(
      at: insightText(json['at'], 64),
      cpuPercent: insightDouble(json['cpu_percent'])
          .clamp(0.0, 100.0)
          .toDouble(),
      memoryPercent: insightDouble(json['memory_percent'])
          .clamp(0.0, 100.0)
          .toDouble(),
      processRssBytes: insightInt(json['process_rss_bytes']),
    );
  }

  final String at;
  final double cpuPercent;
  final double memoryPercent;
  final int processRssBytes;
}

class UsageInsights {
  const UsageInsights({
    this.days = 7,
    this.scope = 'me',
    this.channels = const [],
    this.tunnel = const InsightTunnel(),
    this.relayOnline = false,
    this.models = const [],
    this.resources,
  });

  factory UsageInsights.fromJson(Map<String, dynamic> json) {
    final resources = json['resources'];
    final days = insightInt(json['days']);
    return UsageInsights(
      days: days <= 0 ? 7 : days,
      scope: insightText(json['scope'], 16),
      channels: [
        for (final row in insightRows(json['channels']))
          InsightChannel.fromJson(row),
      ],
      tunnel: InsightTunnel.fromJson(json['tunnel']),
      relayOnline: json['relay_online'] == true,
      models: [
        for (final row in insightRows(json['models']))
          InsightModel.fromJson(row),
      ],
      // Admin only: a list when the server sent one, null for members.
      resources: resources is List
          ? [
              for (final row in insightRows(resources))
                InsightResourceSample.fromJson(row),
            ]
          : null,
    );
  }

  final int days;
  final String scope;
  final List<InsightChannel> channels;
  final InsightTunnel tunnel;
  final bool relayOnline;
  final List<InsightModel> models;
  final List<InsightResourceSample>? resources;

  /// Replies started in this period, across every channel.
  int get replies => channels.fold(0, (sum, c) => sum + c.turns);

  /// Tokens used in this period, across every channel.
  int get tokens => channels.fold(0, (sum, c) => sum + c.tokens);
}

/// Whole-number share of [part] in [total], 0 when there is no total.
int sharePercent(int part, int total) {
  if (total <= 0 || part <= 0) {
    return 0;
  }
  final rounded = (part * 100 / total).round();
  return rounded > 100 ? 100 : rounded;
}

/// What a channel is called on the card.
String channelLabel(String channel) {
  return switch (channel) {
    'desktop' => 'This PC app',
    'mobile' => 'Phone app',
    'telegram' => 'Telegram',
    'whatsapp' => 'WhatsApp',
    'app' => 'App (before 0.67)',
    'api' => 'API and scripts',
    _ => 'Schedules and watches',
  };
}

/// "850 B", "2 KB", "3.0 MB", "1.2 GB".
String humanBytes(int bytes) {
  final value = bytes < 0 ? 0.0 : bytes.toDouble();
  const kb = 1024.0;
  if (value >= kb * kb * kb) {
    return '${(value / (kb * kb * kb)).toStringAsFixed(1)} GB';
  }
  if (value >= kb * kb) {
    return '${(value / (kb * kb)).toStringAsFixed(1)} MB';
  }
  if (value >= kb) {
    return '${(value / kb).toStringAsFixed(0)} KB';
  }
  return '${value.toStringAsFixed(0)} B';
}

/// "850 ms", "2.4 s", or a dash when there is no sample.
String humanLatency(int ms) {
  if (ms <= 0) {
    return '—';
  }
  if (ms < 1000) {
    return '$ms ms';
  }
  return '${(ms / 1000).toStringAsFixed(1)} s';
}

/// "1,234,567".
String formatCount(int value) {
  final digits = value.abs().toString();
  final buffer = StringBuffer(value < 0 ? '-' : '');
  for (var i = 0; i < digits.length; i++) {
    if (i > 0 && (digits.length - i) % 3 == 0) {
      buffer.write(',');
    }
    buffer.write(digits[i]);
  }
  return buffer.toString();
}
