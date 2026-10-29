import 'dart:async';

import 'package:flutter/material.dart';

import '../api_client.dart';
import '../models.dart';
import '../session.dart';

class ApprovalsTab extends StatefulWidget {
  const ApprovalsTab({super.key, required this.session});

  final OwnerSession session;

  @override
  State<ApprovalsTab> createState() => _ApprovalsTabState();
}

class _ApprovalsTabState extends State<ApprovalsTab> {
  List<ApprovalItem> _items = [];
  String? _error;
  Timer? _timer;
  DateTime _lastFetch = DateTime.fromMillisecondsSinceEpoch(0);

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 5), (_) => _load(silent: true));
    _load();
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _load({bool silent = false}) async {
    if (!widget.session.connected) {
      return;
    }
    final elapsed = DateTime.now().difference(_lastFetch);
    if (elapsed < const Duration(seconds: 3)) {
      await Future<void>.delayed(const Duration(seconds: 3) - elapsed);
    }
    _lastFetch = DateTime.now();
    try {
      final items = await widget.session.client.approvals();
      if (!mounted) {
        return;
      }
      setState(() {
        _items = items;
        if (!silent) {
          _error = null;
        }
      });
    } on ApiException catch (e) {
      if (!silent) {
        setState(() => _error = e.messageForUser());
      }
    }
  }

  Future<void> _decide(ApprovalItem item, ApprovalDecision decision) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(decision.label),
        content: Text(item.summary),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Confirm')),
        ],
      ),
    );
    if (ok != true || !mounted) {
      return;
    }
    try {
      await widget.session.run((c) => c.decideApproval(item.id, decision));
      await _load();
    } on ApiException catch (e) {
      setState(() => _error = e.messageForUser());
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Text(_error!, textAlign: TextAlign.center),
        ),
      );
    }
    if (_items.isEmpty) {
      return RefreshIndicator(
        onRefresh: () => _load(),
        child: ListView(
          children: const [
            SizedBox(height: 120),
            Center(child: Text('No pending approvals')),
          ],
        ),
      );
    }
    return RefreshIndicator(
      onRefresh: () => _load(),
      child: ListView.builder(
        padding: const EdgeInsets.all(12),
        itemCount: _items.length,
        itemBuilder: (context, index) {
          final a = _items[index];
          return Card(
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(a.tool, style: Theme.of(context).textTheme.titleSmall),
                  const SizedBox(height: 4),
                  Text(a.summary),
                  const SizedBox(height: 4),
                  Text(
                    'Expires ${a.expiresAt}',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  const SizedBox(height: 8),
                  Wrap(
                    spacing: 8,
                    children: ApprovalDecision.values.map((d) {
                      return FilledButton.tonal(
                        onPressed: widget.session.busy
                            ? null
                            : () => _decide(a, d),
                        child: Text(d.label),
                      );
                    }).toList(),
                  ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}
