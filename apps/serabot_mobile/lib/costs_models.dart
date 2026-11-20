// Costs and budget (`GET /v1/usage/costs`, `PUT /v1/usage/budget`; 0.69 and
// the 0.70 `totals` and `plans` fields). Estimates from the owner's price
// table, never the provider's bill. Every field is optional on the wire: a
// missing or wrong-typed value falls back to its default, and the 0.70
// sections stay hidden when the server does not send them.

import 'usage_insights_models.dart' show insightInt, insightRows, insightText;

Map<String, dynamic> _map(Object? v) =>
    v is Map<String, dynamic> ? v : const <String, dynamic>{};

/// A finite JSON number as a double, or null.
double? _money(Object? v) => v is num && v.isFinite ? v.toDouble() : null;

double _moneyOr0(Object? v) => _money(v) ?? 0;

/// One model's usage this month. [onPlan] is true when a flat plan covers it
/// (the server sends `plan`, a name or a flag), so it has no per-token price.
class CostLine {
  const CostLine({
    required this.provider,
    required this.model,
    this.promptTokens = 0,
    this.completionTokens = 0,
    this.cachedTokens = 0,
    this.usd,
    this.onPlan = false,
    this.apiEquivalentUsd,
  });

  final String provider;
  final String model;
  final int promptTokens;
  final int completionTokens;
  final int cachedTokens;

  /// Null when the model has no price in the table.
  final double? usd;
  final bool onPlan;

  /// What this usage would cost at API prices (0.70). Null when unknown.
  final double? apiEquivalentUsd;

  factory CostLine.fromJson(Map<String, dynamic> json) {
    final plan = json['plan'];
    return CostLine(
      provider: insightText(json['provider'], 60),
      model: insightText(json['model'], 120),
      promptTokens: insightInt(json['prompt_tokens']),
      completionTokens: insightInt(json['completion_tokens']),
      cachedTokens: insightInt(json['cached_tokens']),
      usd: _money(json['usd']),
      onPlan: plan == true || (plan is String && plan.trim().isNotEmpty),
      apiEquivalentUsd: _money(json['api_equivalent_usd']),
    );
  }
}

/// A row of the owner's price table, USD per million tokens.
class CostPrice {
  const CostPrice({
    required this.provider,
    required this.model,
    required this.inputPerM,
    required this.outputPerM,
  });

  final String provider;
  final String model;
  final double inputPerM;
  final double outputPerM;

  factory CostPrice.fromJson(Map<String, dynamic> json) => CostPrice(
    provider: insightText(json['provider'], 60),
    model: insightText(json['model'], 120),
    inputPerM: _moneyOr0(json['input_per_m']),
    outputPerM: _moneyOr0(json['output_per_m']),
  );
}

/// Token totals for one period, in millions, and their estimated cost.
class CostPeriodTotals {
  const CostPeriodTotals({
    this.inputM = 0,
    this.cachedM = 0,
    this.outputM = 0,
    this.usd = 0,
    this.planApiEquivalentUsd = 0,
  });

  final double inputM;
  final double cachedM;
  final double outputM;
  final double usd;

  /// What the plan-covered usage would cost at API prices (0.70).
  final double planApiEquivalentUsd;

  factory CostPeriodTotals.fromJson(Map<String, dynamic> json) =>
      CostPeriodTotals(
        inputM: _moneyOr0(json['input_m']),
        cachedM: _moneyOr0(json['cached_m']),
        outputM: _moneyOr0(json['output_m']),
        usd: _moneyOr0(json['usd']),
        planApiEquivalentUsd: _moneyOr0(json['plan_api_equivalent_usd']),
      );
}

class CostTotals {
  const CostTotals({this.month, this.allTime});

  final CostPeriodTotals? month;
  final CostPeriodTotals? allTime;

  factory CostTotals.fromJson(Map<String, dynamic> json) => CostTotals(
    month: json['month'] is Map<String, dynamic>
        ? CostPeriodTotals.fromJson(_map(json['month']))
        : null,
    allTime: json['all_time'] is Map<String, dynamic>
        ? CostPeriodTotals.fromJson(_map(json['all_time']))
        : null,
  );
}

/// A flat plan compared with what the same usage would cost at API prices.
class PlanCost {
  const PlanCost({
    required this.provider,
    this.monthlyUsd,
    this.apiEquivalentUsd = 0,
    this.unpricedTokens = 0,
    this.savingUsd,
  });

  final String provider;

  /// The plan price a month; null until the owner sets it.
  final double? monthlyUsd;
  final double apiEquivalentUsd;

  /// Tokens with no API price, so the API comparison is partial.
  final int unpricedTokens;

  /// API equivalent minus the plan price; null without a plan price.
  final double? savingUsd;

  factory PlanCost.fromJson(Map<String, dynamic> json) => PlanCost(
    provider: insightText(json['provider'], 60),
    monthlyUsd: _money(json['monthly_usd']),
    apiEquivalentUsd: _moneyOr0(json['api_equivalent_usd']),
    unpricedTokens: insightInt(json['unpriced_tokens']),
    savingUsd: _money(json['saving_usd']),
  );
}

/// The body of `PUT /v1/usage/plans`: a plan price per provider, null to
/// clear it.
Map<String, dynamic> plansBody(Map<String, double?> monthlyByProvider) => {
  'plans': [
    for (final e in monthlyByProvider.entries)
      {'provider': e.key, 'monthly_usd': e.value},
  ],
};

class CostsReport {
  const CostsReport({
    this.monthUsd = 0,
    this.monthUnpricedTokens = 0,
    this.budgetUsd,
    this.budgetLevel,
    this.lines = const [],
    this.prices = const [],
    this.totals,
    this.plans,
  });

  final double monthUsd;
  final int monthUnpricedTokens;

  /// Null when no budget is set.
  final double? budgetUsd;

  /// 80 or 100 when the month has crossed that share of the budget.
  final int? budgetLevel;
  final List<CostLine> lines;
  final List<CostPrice> prices;

  /// 0.70. Null when the server did not send it.
  final CostTotals? totals;

  /// 0.70. Null when the server did not send it.
  final List<PlanCost>? plans;

  /// Share of the budget used, 0 to 1. Null without a positive budget.
  double? get budgetFraction {
    final budget = budgetUsd;
    if (budget == null || budget <= 0) {
      return null;
    }
    return (monthUsd / budget).clamp(0.0, 1.0);
  }

  factory CostsReport.fromJson(Map<String, dynamic> json) {
    final level = json['budget_level'];
    final rawPlans = json['plans'];
    final rawTotals = json['totals'];
    return CostsReport(
      monthUsd: _moneyOr0(json['month_usd']),
      monthUnpricedTokens: insightInt(json['month_unpriced_tokens']),
      budgetUsd: _money(json['budget_usd']),
      budgetLevel: level is num ? insightInt(level) : null,
      lines: insightRows(json['month_lines']).map(CostLine.fromJson).toList(),
      prices: insightRows(json['prices']).map(CostPrice.fromJson).toList(),
      totals: rawTotals is Map<String, dynamic>
          ? CostTotals.fromJson(rawTotals)
          : null,
      plans: rawPlans is List
          ? insightRows(rawPlans).map(PlanCost.fromJson).toList()
          : null,
    );
  }
}

/// "$3.21". Whole dollars keep their cents so a column of prices lines up.
String usdText(double value) {
  final safe = value.isFinite ? value : 0.0;
  final sign = safe < 0 ? '-' : '';
  return '$sign\$${safe.abs().toStringAsFixed(2)}';
}

/// Tokens in millions with two decimals: "1.20 M".
String millionsText(num tokens) {
  final value = tokens.isFinite ? tokens / 1000000 : 0.0;
  return '${value.toStringAsFixed(2)} M';
}
