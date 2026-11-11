import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../workflows_models.dart';
import '../widgets.dart';

/// Read-only conversation tool timeline (`GET /v1/traces`).
Future<void> showConversationTraceSheet(
  BuildContext context, {
  required ApiClient client,
  required String conversationId,
  String? title,
}) async {
  await showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => _TraceSheet(
      client: client,
      conversationId: conversationId,
      title: title,
    ),
  );
}

class _TraceSheet extends StatefulWidget {
  const _TraceSheet({
    required this.client,
    required this.conversationId,
    this.title,
  });

  final ApiClient client;
  final String conversationId;
  final String? title;

  @override
  State<_TraceSheet> createState() => _TraceSheetState();
}

class _TraceSheetState extends State<_TraceSheet> {
  bool _loading = true;
  String? _error;
  ConversationTracePage? _page;

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
      final page = await widget.client.conversationTraces(widget.conversationId);
      if (!mounted) {
        return;
      }
      setState(() {
        _page = page;
        _loading = false;
        if (page == null) {
          _error = 'This server does not expose /v1/traces yet.';
        }
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

  @override
  Widget build(BuildContext context) {
    final headline = widget.title ?? widget.conversationId;
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.72,
      minChildSize: 0.4,
      maxChildSize: 0.95,
      builder: (context, scrollController) {
        return Material(
          color: Theme.of(context).colorScheme.surface,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 8, 8),
                child: Row(
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text('Tool trace',
                              style: Theme.of(context).textTheme.titleMedium),
                          Text(
                            headline,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: Theme.of(context).textTheme.bodySmall,
                          ),
                        ],
                      ),
                    ),
                    IconButton(
                      tooltip: 'Refresh',
                      onPressed: _loading ? null : _load,
                      icon: const Icon(Icons.refresh),
                    ),
                  ],
                ),
              ),
              const Divider(height: 1),
              Expanded(
                child: _body(context, scrollController),
              ),
            ],
          ),
        );
      },
    );
  }

  Widget _body(BuildContext context, ScrollController scrollController) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Padding(
        padding: const EdgeInsets.all(16),
        child: ErrorBanner(_error!, onRetry: _load),
      );
    }
    final traces = _page?.traces ?? const [];
    if (traces.isEmpty) {
      return const Center(child: Text('No tool events for this chat yet.'));
    }
    return ListView.builder(
      controller: scrollController,
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 24),
      itemCount: traces.length,
      itemBuilder: (context, i) => _TraceRow(trace: traces[i]),
    );
  }
}

class _TraceRow extends StatelessWidget {
  const _TraceRow({required this.trace});
  final ConversationTrace trace;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final subtitle = <String>[
      if (trace.tool != null) trace.tool!,
      if (trace.destination != null) trace.destination!,
      if (trace.elapsedMs != null) '${trace.elapsedMs} ms',
      if (trace.canonicalHash != null) 'hash ${trace.canonicalHash}',
      if (trace.artifactHash != null) 'artifact ${trace.artifactHash}',
    ].join(' \u00b7 ');
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      elevation: 0,
      color: scheme.surfaceContainerHighest.withValues(alpha: 0.45),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(trace.event, style: Theme.of(context).textTheme.titleSmall),
            const SizedBox(height: 4),
            Text(trace.at, style: Theme.of(context).textTheme.bodySmall),
            if (subtitle.isNotEmpty) ...[
              const SizedBox(height: 6),
              Text(
                subtitle,
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
