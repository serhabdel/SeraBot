import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_models.dart';
import '../usage_insights_models.dart';
import '../widgets.dart';

/// Usage insights (`GET /v1/usage/insights` and `GET /v1/usage`, 0.67): where
/// the member talks to their bots, the PC tunnel, model speed and reliability,
/// and, for the admin, server load. Mirrors the desktop Usage tab. Counts only.
class UsageInsightsScreen extends StatefulWidget {
  const UsageInsightsScreen({super.key, required this.app});

  final AppState app;

  @override
  State<UsageInsightsScreen> createState() => _UsageInsightsScreenState();
}

class _UsageInsightsScreenState extends State<UsageInsightsScreen> {
  int _days = 7;
  bool _loading = true;
  String? _error;
  UsageInsights? _insights;
  UsageReport? _report;

  /// Guards against an older response landing after a newer one.
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final generation = ++_generation;
    final days = _days;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final client = widget.app.client;
      final insights = await client.usageInsights(days: days);
      final report = await client.usage(days: days);
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _insights = insights;
        _report = report;
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  void _setDays(int days) {
    if (days == _days) {
      return;
    }
    setState(() => _days = days);
    _load();
  }

  @override
  Widget build(BuildContext context) {
    final insights = _insights;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Usage insights'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            Center(
              child: SegmentedButton<int>(
                key: const Key('usage-days'),
                segments: const [
                  ButtonSegment(value: 7, label: Text('7 days')),
                  ButtonSegment(value: 30, label: Text('30 days')),
                ],
                selected: {_days},
                onSelectionChanged: _loading ? null : (s) => _setDays(s.first),
              ),
            ),
            const SizedBox(height: 12),
            if (_error != null) ...[
              ErrorBanner(_error!, onRetry: _load),
              const SizedBox(height: 12),
            ],
            if (insights == null && _error == null)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 48),
                child: Center(child: CircularProgressIndicator()),
              )
            else if (insights != null) ...[
              _totals(context, insights),
              const SizedBox(height: 12),
              _channelsCard(context, insights),
              const SizedBox(height: 12),
              _tunnelCard(context, insights),
              const SizedBox(height: 12),
              _modelsCard(context, insights),
              if (insights.resources != null) ...[
                const SizedBox(height: 12),
                _loadCard(context, insights.resources!),
              ],
              const SizedBox(height: 12),
              Text(
                'Last ${insights.days} days. Counts and sizes only, never message text.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _totals(BuildContext context, UsageInsights insights) {
    final report = _report;
    final tokens = report?.totals.totalTokens ?? insights.tokens;
    return Row(
      key: const Key('usage-totals'),
      children: [
        Expanded(
          child: _StatTile(
            label: 'Replies',
            value: formatCount(insights.replies),
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: _StatTile(label: 'Tokens', value: formatCount(tokens)),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: _StatTile(
            label: 'Tool runs',
            value: formatCount(report?.toolRuns ?? 0),
          ),
        ),
      ],
    );
  }

  Widget _channelsCard(BuildContext context, UsageInsights insights) {
    final total = insights.replies;
    return _Card(
      title: 'Where you talk to your bots',
      child: insights.channels.isEmpty
          ? const Text('No turns in this period.')
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                for (final c in insights.channels)
                  _ChannelRow(channel: c, total: total),
              ],
            ),
    );
  }

  Widget _tunnelCard(BuildContext context, UsageInsights insights) {
    final tunnel = insights.tunnel;
    final theme = Theme.of(context);
    final idle = tunnel.tunnels == 0 && tunnel.totalCalls == 0;
    return _Card(
      title: 'PC tunnel',
      trailing: Chip(
        avatar: Icon(
          insights.relayOnline ? Icons.link : Icons.link_off,
          size: 16,
        ),
        label: Text(insights.relayOnline ? 'Relaying' : 'Not relaying'),
      ),
      child: idle
          ? const Text(
              'Nothing went through this PC in this period. Turn the network '
              'relay on in the desktop app to use it.',
            )
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Expanded(
                      child: _StatTile(
                        label: 'Model calls via this PC',
                        value: '${tunnel.relayedPercent}%',
                        caption:
                            '${formatCount(tunnel.relayedCalls)} of '
                            '${formatCount(tunnel.totalCalls)}',
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: _StatTile(
                        label: 'Downloaded and sent',
                        value: humanBytes(tunnel.bytes),
                      ),
                    ),
                  ],
                ),
                if (tunnel.perDay.isNotEmpty) ...[
                  const SizedBox(height: 12),
                  Text('Traffic per day', style: theme.textTheme.labelLarge),
                  const SizedBox(height: 4),
                  _TrafficBars(days: tunnel.perDay),
                ],
                if (tunnel.perHost.isNotEmpty) ...[
                  const SizedBox(height: 12),
                  Text('By host', style: theme.textTheme.labelLarge),
                  for (final h in tunnel.perHost)
                    ListTile(
                      dense: true,
                      contentPadding: EdgeInsets.zero,
                      title: Text(h.host, overflow: TextOverflow.ellipsis),
                      subtitle: Text(
                        '${formatCount(h.tunnels)} tunnels · '
                        'avg ${h.avgSeconds} s',
                      ),
                      trailing: Text(humanBytes(h.bytes)),
                    ),
                ],
              ],
            ),
    );
  }

  Widget _modelsCard(BuildContext context, UsageInsights insights) {
    return _Card(
      title: 'Model speed and reliability',
      child: insights.models.isEmpty
          ? const Text('No model calls in this period.')
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                for (final m in insights.models)
                  ListTile(
                    key: ValueKey('model-${m.provider}-${m.model}'),
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    title: Text(
                      m.model.isEmpty ? 'Unnamed model' : m.model,
                      overflow: TextOverflow.ellipsis,
                    ),
                    subtitle: Text(
                      '${m.provider.isEmpty ? 'provider' : m.provider} · '
                      '${formatCount(m.calls)} calls · '
                      'avg ${humanLatency(m.avgLatencyMs)} · '
                      'slowest 5% ${humanLatency(m.p95LatencyMs)} · '
                      '${m.relayedPercent}% via PC · '
                      '${m.failovers} failovers',
                    ),
                  ),
              ],
            ),
    );
  }

  Widget _loadCard(BuildContext context, List<InsightResourceSample> samples) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    if (samples.isEmpty) {
      return _Card(
        title: 'Server load',
        child: const Text(
          'Sampling every 5 minutes since the server started. Check back soon.',
        ),
      );
    }
    final cpu = [for (final s in samples) s.cpuPercent];
    final memory = [for (final s in samples) s.memoryPercent];
    return _Card(
      title: 'Server load',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Spark(
            label: 'CPU',
            values: cpu,
            color: scheme.primary,
            gridColor: scheme.outlineVariant,
          ),
          const SizedBox(height: 12),
          _Spark(
            label: 'Memory',
            values: memory,
            color: scheme.tertiary,
            gridColor: scheme.outlineVariant,
          ),
        ],
      ),
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({required this.title, required this.child, this.trailing});

  final String title;
  final Widget child;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    title,
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ),
                ?trailing,
              ],
            ),
            const SizedBox(height: 8),
            child,
          ],
        ),
      ),
    );
  }
}

class _StatTile extends StatelessWidget {
  const _StatTile({required this.label, required this.value, this.caption});

  final String label;
  final String value;
  final String? caption;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Semantics(
      label: caption == null ? '$label, $value' : '$label, $value, $caption',
      child: Container(
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: theme.colorScheme.surfaceContainerHighest.withValues(
            alpha: 0.5,
          ),
          borderRadius: BorderRadius.circular(12),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(value, style: theme.textTheme.titleLarge),
            Text(label, style: theme.textTheme.bodySmall),
            if (caption != null)
              Text(caption!, style: theme.textTheme.bodySmall),
          ],
        ),
      ),
    );
  }
}

class _ChannelRow extends StatelessWidget {
  const _ChannelRow({required this.channel, required this.total});

  final InsightChannel channel;
  final int total;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final share = sharePercent(channel.turns, total);
    final label = channelLabel(channel.channel);
    return Semantics(
      label: '$label, ${channel.turns} replies, $share percent',
      excludeSemantics: true,
      child: Padding(
        padding: const EdgeInsets.only(bottom: 10),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(child: Text(label)),
                Text('${formatCount(channel.turns)} · $share%'),
              ],
            ),
            const SizedBox(height: 4),
            LinearProgressIndicator(
              value: share / 100,
              minHeight: 8,
              borderRadius: BorderRadius.circular(4),
            ),
            Text(
              '${formatCount(channel.tokens)} tokens',
              style: theme.textTheme.bodySmall,
            ),
          ],
        ),
      ),
    );
  }
}

/// Bars for the tunnel's traffic per day, scaled to the busiest day.
class _TrafficBars extends StatelessWidget {
  const _TrafficBars({required this.days});

  final List<InsightTunnelDay> days;

  @override
  Widget build(BuildContext context) {
    final peak = days.fold<int>(0, (m, d) => d.bytes > m ? d.bytes : m);
    final heights = [for (final d in days) peak == 0 ? 0.0 : d.bytes / peak];
    final total = days.fold<int>(0, (sum, d) => sum + d.bytes);
    return Semantics(
      label: 'Traffic per day, ${humanBytes(total)} in total',
      image: true,
      child: SizedBox(
        height: 56,
        width: double.infinity,
        child: CustomPaint(
          painter: _BarsPainter(
            values: heights,
            color: Theme.of(context).colorScheme.primary,
          ),
        ),
      ),
    );
  }
}

class _Spark extends StatelessWidget {
  const _Spark({
    required this.label,
    required this.values,
    required this.color,
    required this.gridColor,
  });

  final String label;
  final List<double> values;
  final Color color;
  final Color gridColor;

  @override
  Widget build(BuildContext context) {
    final latest = values.isEmpty ? 0 : values.last.round();
    return Semantics(
      label: '$label over time, latest $latest percent',
      image: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(child: Text(label)),
              Text('$latest%'),
            ],
          ),
          SizedBox(
            height: 56,
            width: double.infinity,
            child: CustomPaint(
              painter: _SparkPainter(
                values: values,
                color: color,
                gridColor: gridColor,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// A line through 0 to 100 percent values, oldest on the left.
class _SparkPainter extends CustomPainter {
  const _SparkPainter({
    required this.values,
    required this.color,
    required this.gridColor,
  });

  final List<double> values;
  final Color color;
  final Color gridColor;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawLine(
      Offset(0, size.height),
      Offset(size.width, size.height),
      Paint()
        ..color = gridColor
        ..strokeWidth = 1,
    );
    if (values.length < 2) {
      return;
    }
    final step = size.width / (values.length - 1);
    final path = Path();
    for (var i = 0; i < values.length; i++) {
      final level = values[i].clamp(0.0, 100.0).toDouble() / 100.0;
      final point = Offset(
        i.toDouble() * step,
        size.height - level * size.height,
      );
      if (i == 0) {
        path.moveTo(point.dx, point.dy);
      } else {
        path.lineTo(point.dx, point.dy);
      }
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = color
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeJoin = StrokeJoin.round,
    );
  }

  @override
  bool shouldRepaint(_SparkPainter old) {
    return old.values != values ||
        old.color != color ||
        old.gridColor != gridColor;
  }
}

/// Vertical bars; each value is 0 to 1 of the full height.
class _BarsPainter extends CustomPainter {
  const _BarsPainter({required this.values, required this.color});

  final List<double> values;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    if (values.isEmpty) {
      return;
    }
    final gap = values.length > 30 ? 1.0 : 3.0;
    final width = (size.width - gap * (values.length - 1)) / values.length;
    final barWidth = width < 1 ? 1.0 : width;
    final paint = Paint()..color = color;
    for (var i = 0; i < values.length; i++) {
      final level = values[i].clamp(0.0, 1.0).toDouble();
      final scaled = level * size.height;
      final height = scaled < 2 ? 2.0 : scaled;
      final rect = Rect.fromLTWH(
        i.toDouble() * (barWidth + gap),
        size.height - height,
        barWidth,
        height,
      );
      canvas.drawRRect(
        RRect.fromRectAndRadius(rect, const Radius.circular(2)),
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_BarsPainter old) {
    return old.values != values || old.color != color;
  }
}
