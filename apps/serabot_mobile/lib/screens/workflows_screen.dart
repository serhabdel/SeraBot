import 'package:flutter/material.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../workflows_models.dart';
import '../widgets.dart';
import 'trace_sheet.dart';

/// Read-only Workflows parity with desktop (routines, source health, active plans).
class WorkflowsScreen extends StatefulWidget {
  const WorkflowsScreen({super.key, required this.app});

  final AppState app;

  @override
  State<WorkflowsScreen> createState() => _WorkflowsScreenState();
}

class _WorkflowsScreenState extends State<WorkflowsScreen> {
  bool _loading = true;
  String? _error;
  WorkflowsSnapshot _snapshot = const WorkflowsSnapshot();

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final snap = await widget.app.client.workflowsSnapshot();
      if (!mounted) {
        return;
      }
      setState(() {
        _snapshot = snap;
        _loading = false;
      });
    } on ApiException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.messageForUser();
          _loading = false;
        });
      }
    }
  }

  void _openTraces(String conversationId) {
    showConversationTraceSheet(
      context,
      client: widget.app.client,
      conversationId: conversationId,
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Workflows'),
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
        child: _body(context),
      ),
    );
  }

  Widget _body(BuildContext context) {
    if (_loading && _error == null) {
      return ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        children: const [
          SizedBox(height: 120),
          Center(child: CircularProgressIndicator()),
        ],
      );
    }
    final children = <Widget>[];
    if (_error != null) {
      children.add(Padding(
        padding: const EdgeInsets.all(16),
        child: ErrorBanner(_error!, onRetry: _load),
      ));
    }
    children.addAll([
      _section(
        context,
        title: 'Scheduled routines',
        unsupported: !_snapshot.routinesSupported,
        unsupportedMessage: 'This server does not expose /v1/routines yet.',
        emptyMessage: 'No routines configured.',
        isEmpty: _snapshot.routines.isEmpty,
        child: _routineList(_snapshot.routines),
      ),
      _section(
        context,
        title: 'Source health',
        unsupported: !_snapshot.sourcesSupported,
        unsupportedMessage: 'This server does not expose /v1/monitoring/sources yet.',
        emptyMessage: 'No watcher sources configured.',
        isEmpty: _snapshot.sources.isEmpty,
        child: _sourceList(_snapshot.sources),
      ),
      _section(
        context,
        title: 'Active plans',
        unsupported: !_snapshot.activeSupported,
        unsupportedMessage: 'This server does not expose /v1/workflows/active yet.',
        emptyMessage: 'No incomplete plans.',
        isEmpty: _snapshot.active.isEmpty,
        child: _activeList(_snapshot.active),
      ),
      const SizedBox(height: 24),
    ]);
    return ListView(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      children: children,
    );
  }

  Widget _section(
    BuildContext context, {
    required String title,
    required bool unsupported,
    required String unsupportedMessage,
    required String emptyMessage,
    required bool isEmpty,
    required Widget child,
  }) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 12),
        Text(title, style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 8),
        if (unsupported)
          Text(unsupportedMessage,
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                    color: Theme.of(context).colorScheme.onSurfaceVariant,
                  ))
        else if (isEmpty)
          Text(emptyMessage,
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                    color: Theme.of(context).colorScheme.onSurfaceVariant,
                  ))
        else
          child,
      ],
    );
  }

  Widget _routineList(List<Routine> routines) {
    return Column(
      children: [
        for (final r in routines) _RoutineTile(routine: r),
      ],
    );
  }

  Widget _sourceList(List<MonitoringSource> sources) {
    return Column(
      children: [
        for (final s in sources) _SourceTile(source: s),
      ],
    );
  }

  Widget _activeList(List<ActiveWorkflow> plans) {
    return Column(
      children: [
        for (final p in plans)
          _ActiveTile(
            plan: p,
            onTraces: () => _openTraces(p.conversationId),
          ),
      ],
    );
  }
}

class _RoutineTile extends StatelessWidget {
  const _RoutineTile({required this.routine});
  final Routine routine;

  @override
  Widget build(BuildContext context) {
    final schedule = switch ((routine.time, routine.timezone)) {
      (final t?, final z?) => '$t ($z)',
      (final t?, null) => t,
      _ => '\u2014',
    };
    final scheme = Theme.of(context).colorScheme;
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      child: ListTile(
        title: Text(routine.name, maxLines: 1, overflow: TextOverflow.ellipsis),
        subtitle: Text(
          'Next ${formatNextRun(routine.nextRunAt)}\n$schedule'
          '${routine.lastRunFor != null ? '\nLast ${routine.lastRunFor}' : ''}',
        ),
        isThreeLine: routine.lastRunFor != null,
        trailing: _StatusPill(
          label: routine.enabled ? 'On' : 'Off',
          color: routine.enabled ? scheme.primary : scheme.outline,
        ),
      ),
    );
  }
}

class _SourceTile extends StatelessWidget {
  const _SourceTile({required this.source});
  final MonitoringSource source;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final run = source.lastRun;
    final status = switch (run?.ok) {
      true => 'OK \u00b7 ${run!.itemsSeen.clamp(0, 1 << 30)} items',
      false => run?.error ?? 'Error',
      null => 'No run yet',
    };
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      child: ListTile(
        title: Text(source.name, maxLines: 1, overflow: TextOverflow.ellipsis),
        subtitle: Text('${source.kind}\n$status'),
        isThreeLine: true,
        trailing: _StatusPill(
          label: source.enabled ? 'Enabled' : 'Disabled',
          color: source.enabled ? scheme.primary : scheme.outline,
        ),
      ),
    );
  }
}

class _ActiveTile extends StatelessWidget {
  const _ActiveTile({required this.plan, required this.onTraces});

  final ActiveWorkflow plan;
  final VoidCallback onTraces;

  @override
  Widget build(BuildContext context) {
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      child: ListTile(
        title: Text(plan.goal, maxLines: 2, overflow: TextOverflow.ellipsis),
        subtitle: Text(
          '${plan.conversationId}\n'
          '${planProgress(plan.done, plan.total)}'
          '${plan.updatedAt != null ? ' \u00b7 Updated ${plan.updatedAt}' : ''}',
        ),
        isThreeLine: true,
        trailing: IconButton(
          tooltip: 'Tool trace',
          onPressed: onTraces,
          icon: const Icon(Icons.timeline_outlined),
        ),
      ),
    );
  }
}

class _StatusPill extends StatelessWidget {
  const _StatusPill({required this.label, required this.color});
  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
      decoration: BoxDecoration(
        border: Border.all(color: color),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(label, style: TextStyle(fontSize: 12, color: color)),
    );
  }
}
