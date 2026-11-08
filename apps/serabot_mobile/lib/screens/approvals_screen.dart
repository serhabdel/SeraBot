import 'package:flutter/material.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../models.dart';
import '../widgets.dart';

/// Pending approvals, decided through the same binding API as the desktop
/// (`POST /v1/approvals/{id}/decide`).
class ApprovalsScreen extends StatefulWidget {
  const ApprovalsScreen({super.key, required this.app});
  final AppState app;

  @override
  State<ApprovalsScreen> createState() => _ApprovalsScreenState();
}

class _ApprovalsScreenState extends State<ApprovalsScreen> {
  List<ApprovalItem> _items = const [];
  bool _loading = true;
  String? _error;
  String? _busyId;
  int _lastCount = -1;

  @override
  void initState() {
    super.initState();
    widget.app.addListener(_onApp);
    _load();
  }

  @override
  void dispose() {
    widget.app.removeListener(_onApp);
    super.dispose();
  }

  void _onApp() {
    if (widget.app.pendingApprovals != _lastCount) {
      _load();
    }
  }

  Future<void> _load() async {
    try {
      final items = await widget.app.client.approvals();
      if (!mounted) {
        return;
      }
      setState(() {
        _items = items;
        _lastCount = items.length;
        _error = null;
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

  Future<void> _decide(ApprovalItem a, ApprovalDecision d) async {
    setState(() => _busyId = a.id);
    try {
      await widget.app.client.decideApproval(a.id, d);
      await widget.app.refreshApprovals();
      await _load();
    } on ApiException catch (e) {
      if (mounted) {
        setState(() => _error = e.messageForUser());
      }
    } finally {
      if (mounted) {
        setState(() => _busyId = null);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Approvals')),
      body: RefreshIndicator(
        onRefresh: _load,
        child: _loading
            ? const Center(child: CircularProgressIndicator())
            : ListView(
                padding: const EdgeInsets.all(12),
                children: [
                  if (_error != null) ErrorBanner(_error!, onRetry: _load),
                  if (_items.isEmpty && _error == null)
                    const Padding(
                      padding: EdgeInsets.only(top: 120),
                      child: Center(child: Text('Nothing is waiting for you.')),
                    ),
                  for (final a in _items) _card(context, a),
                ],
              ),
      ),
    );
  }

  Widget _card(BuildContext context, ApprovalItem a) {
    final busy = _busyId == a.id;
    return Card(
      key: Key('approval-${a.id}'),
      margin: const EdgeInsets.only(bottom: 10),
      child: Padding(
        padding: const EdgeInsets.all(14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(children: [
              const Icon(Icons.gavel, size: 18),
              const SizedBox(width: 8),
              Expanded(
                child: Text(a.tool, style: Theme.of(context).textTheme.titleSmall),
              ),
            ]),
            const SizedBox(height: 6),
            Text(a.summary),
            const SizedBox(height: 4),
            Text('Expires ${shortTime(a.expiresAt)}',
                style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 8),
            Wrap(spacing: 8, children: [
              FilledButton(
                onPressed: busy ? null : () => _decide(a, ApprovalDecision.once),
                child: const Text('Approve once'),
              ),
              OutlinedButton(
                onPressed: busy ? null : () => _decide(a, ApprovalDecision.always),
                child: const Text('Always allow'),
              ),
              TextButton(
                onPressed: busy ? null : () => _decide(a, ApprovalDecision.deny),
                child: const Text('Deny'),
              ),
            ]),
          ],
        ),
      ),
    );
  }
}
