import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../costs_models.dart';
import '../widgets.dart';

/// Estimated model cost this month, the member's budget, and (0.70) the
/// comparison with flat plans. Mirrors the desktop Usage tab. Estimates only:
/// the provider's own bill is the truth.
class CostsScreen extends StatefulWidget {
  const CostsScreen({super.key, required this.app});

  final AppState app;

  @override
  State<CostsScreen> createState() => _CostsScreenState();
}

class _CostsScreenState extends State<CostsScreen> {
  final _budget = TextEditingController();
  CostsReport? _report;
  String? _error;
  bool _loading = true;
  bool _saving = false;

  /// Guards against an older response landing after a newer one.
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _budget.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final report = await widget.app.client.usageCosts();
      if (!mounted || generation != _generation) {
        return;
      }
      _showReport(report);
      setState(() => _loading = false);
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

  /// Copies the server's budget into the field, without a stale edit lost
  /// on a refresh the member did not ask for.
  void _showReport(CostsReport report) {
    _report = report;
    final budget = report.budgetUsd;
    _budget.text = budget == null ? '' : budget.toStringAsFixed(2);
  }

  void _onSave() {
    final value = double.tryParse(_budget.text.trim().replaceAll(',', '.'));
    if (value == null || !value.isFinite || value <= 0 || value > 1000000) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Enter a monthly budget above \$0, or clear it.'),
        ),
      );
      return;
    }
    _saveBudget(value);
  }

  Future<void> _saveBudget(double? usd) async {
    setState(() => _saving = true);
    try {
      final report = await widget.app.client.setMonthlyBudget(usd);
      if (!mounted) {
        return;
      }
      setState(() {
        _showReport(report);
        _saving = false;
      });
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _saving = false);
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  @override
  Widget build(BuildContext context) {
    final report = _report;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Costs and budget'),
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
            if (_error != null) ...[
              ErrorBanner(_error!, onRetry: _load),
              const SizedBox(height: 12),
            ],
            if (report == null && _error == null)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 48),
                child: Center(child: CircularProgressIndicator()),
              )
            else if (report != null) ...[
              _monthCard(context, report),
              const SizedBox(height: 12),
              _budgetCard(context, report),
              if (report.totals != null) ...[
                const SizedBox(height: 12),
                _totalsCard(context, report.totals!),
              ],
              if (report.plans != null && report.plans!.isNotEmpty) ...[
                const SizedBox(height: 12),
                _plansCard(context, report.plans!),
              ],
              const SizedBox(height: 16),
              Text(
                'By model this month',
                style: Theme.of(context).textTheme.titleSmall,
              ),
              ..._lineTiles(context, report.lines),
              const SizedBox(height: 12),
              Text(
                'Estimates from the owner’s price table. The provider’s '
                'bill is the truth.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _monthCard(BuildContext context, CostsReport report) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('costs-month'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Estimated this month', style: theme.textTheme.bodySmall),
            const SizedBox(height: 4),
            Text(
              usdText(report.monthUsd),
              style: theme.textTheme.headlineMedium,
            ),
            if (report.monthUnpricedTokens > 0) ...[
              const SizedBox(height: 8),
              Text(
                '${millionsText(report.monthUnpricedTokens)} tokens have no '
                'price yet, so they are not counted.',
                style: theme.textTheme.bodySmall,
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _budgetCard(BuildContext context, CostsReport report) {
    final theme = Theme.of(context);
    final budget = report.budgetUsd;
    final fraction = report.budgetFraction;
    final percent = fraction == null ? 0 : (fraction * 100).round();
    final levelText = switch (report.budgetLevel) {
      100 => 'Over budget',
      80 => 'Close to budget',
      _ => 'Within budget',
    };
    return Card(
      key: const Key('costs-budget'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Monthly budget', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            if (budget != null && fraction != null) ...[
              Semantics(
                label: 'Budget used, $percent percent. $levelText.',
                child: LinearProgressIndicator(
                  value: fraction,
                  minHeight: 6,
                  borderRadius: BorderRadius.circular(3),
                ),
              ),
              const SizedBox(height: 6),
              Text(
                '${usdText(report.monthUsd)} of ${usdText(budget)} '
                '($percent%). $levelText.',
                style: theme.textTheme.bodySmall,
              ),
            ] else
              Text(
                'No budget set. Alerts warn at 80% and 100% of it.',
                style: theme.textTheme.bodySmall,
              ),
            const SizedBox(height: 12),
            TextField(
              key: const Key('budget-field'),
              controller: _budget,
              enabled: !_saving,
              keyboardType: const TextInputType.numberWithOptions(
                decimal: true,
              ),
              decoration: const InputDecoration(
                labelText: 'Monthly budget (USD)',
                prefixText: '\$ ',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                FilledButton(
                  key: const Key('budget-save'),
                  onPressed: _saving ? null : _onSave,
                  child: const Text('Save budget'),
                ),
                if (budget != null)
                  TextButton(
                    key: const Key('budget-clear'),
                    onPressed: _saving ? null : () => _saveBudget(null),
                    child: const Text('Clear budget'),
                  ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _totalsCard(BuildContext context, CostTotals totals) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('costs-totals'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Tokens and cost', style: theme.textTheme.titleSmall),
            if (totals.month != null)
              _periodRow(context, 'This month', totals.month!),
            if (totals.allTime != null)
              _periodRow(context, 'All time', totals.allTime!),
          ],
        ),
      ),
    );
  }

  Widget _periodRow(BuildContext context, String label, CostPeriodTotals t) {
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: Text(
        '$label: in ${t.inputM.toStringAsFixed(2)} M, '
        'cached ${t.cachedM.toStringAsFixed(2)} M, '
        'out ${t.outputM.toStringAsFixed(2)} M, '
        '${usdText(t.usd)}',
        style: Theme.of(context).textTheme.bodyMedium,
      ),
    );
  }

  Widget _plansCard(BuildContext context, List<PlanCost> plans) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('costs-plans'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Plans vs API', style: theme.textTheme.titleSmall),
            const SizedBox(height: 4),
            Text(
              'The same usage at API prices, compared with the plan price.',
              style: theme.textTheme.bodySmall,
            ),
            for (final p in plans)
              ListTile(
                dense: true,
                contentPadding: EdgeInsets.zero,
                title: Text(p.provider),
                subtitle: Text(
                  [
                    p.monthlyUsd == null
                        ? 'Plan price not set'
                        : 'Plan ${usdText(p.monthlyUsd!)} a month',
                    'API prices ${usdText(p.apiEquivalentUsd)}',
                    if (p.unpricedTokens > 0)
                      '${millionsText(p.unpricedTokens)} unpriced',
                  ].join(' · '),
                ),
                trailing: Text(
                  p.savingUsd == null
                      ? ''
                      : p.savingUsd! < 0
                      ? 'Costs ${usdText(-p.savingUsd!)} more'
                      : 'Saves ${usdText(p.savingUsd!)}',
                ),
              ),
          ],
        ),
      ),
    );
  }

  List<Widget> _lineTiles(BuildContext context, List<CostLine> lines) {
    if (lines.isEmpty) {
      return const [ListTile(title: Text('No model calls this month.'))];
    }
    return [
      for (final line in lines)
        ListTile(
          key: Key('cost-line-${line.provider}-${line.model}'),
          dense: true,
          title: Text(
            line.model.isEmpty ? 'Unnamed model' : line.model,
            overflow: TextOverflow.ellipsis,
          ),
          subtitle: Text(
            [
              if (line.provider.isNotEmpty) line.provider,
              'In ${millionsText(line.promptTokens)}',
              'Out ${millionsText(line.completionTokens)}',
              if (line.cachedTokens > 0)
                'Cached ${millionsText(line.cachedTokens)}',
            ].join(' · '),
          ),
          trailing: Text(
            line.onPlan
                ? 'plan'
                : (line.usd == null ? 'no price' : usdText(line.usd!)),
          ),
        ),
    ];
  }
}
