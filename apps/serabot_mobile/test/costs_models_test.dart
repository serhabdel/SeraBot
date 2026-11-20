import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/costs_models.dart';

/// Synthetic GET /v1/usage/costs from 0.69: no totals, no plans.
Map<String, dynamic> _legacyBody() => {
  'month_usd': 8.5,
  'month_unpriced_tokens': 2000000,
  'budget_usd': 10,
  'budget_level': 80,
  'month_lines': [
    {
      'provider': 'acme',
      'model': 'synthetic-model',
      'prompt_tokens': 1500000,
      'completion_tokens': 250000,
      'usd': 8.5,
    },
    {
      'provider': 'acme',
      'model': 'unpriced-model',
      'prompt_tokens': 10,
      'completion_tokens': 20,
      'usd': null,
    },
  ],
  'prices': [
    {
      'provider': 'acme',
      'model': 'synthetic-model',
      'input_per_m': 2.5,
      'output_per_m': 10,
    },
  ],
};

/// Synthetic GET /v1/usage/costs from 0.70: totals and plans added.
Map<String, dynamic> _currentBody() => {
  ..._legacyBody(),
  'month_lines': [
    {
      'provider': 'acme',
      'model': 'synthetic-model',
      'prompt_tokens': 1500000,
      'completion_tokens': 250000,
      'cached_tokens': 400000,
      'usd': 8.5,
    },
    {
      'provider': 'planco',
      'model': 'plan-model',
      'prompt_tokens': 900000,
      'completion_tokens': 100000,
      'usd': null,
      'plan': 'Synthetic Plus',
    },
  ],
  'totals': {
    'month': {'input_m': 2.4, 'cached_m': 0.4, 'output_m': 0.35, 'usd': 8.5},
    'all_time': {
      'input_m': 10.0,
      'cached_m': 1.0,
      'output_m': 2.0,
      'usd': 41.25,
    },
  },
  'plans': [
    {
      'provider': 'planco',
      'monthly_usd': 20,
      'api_equivalent_usd': 35.5,
      'saving_usd': 15.5,
    },
  ],
};

void main() {
  group('CostsReport.fromJson without the 0.70 fields', () {
    test('reads the month, budget and lines', () {
      final report = CostsReport.fromJson(_legacyBody());

      expect(report.monthUsd, 8.5);
      expect(report.monthUnpricedTokens, 2000000);
      expect(report.budgetUsd, 10);
      expect(report.budgetLevel, 80);
      expect(report.lines, hasLength(2));
      expect(report.lines.first.usd, 8.5);
      expect(report.lines.last.usd, isNull, reason: 'no price in the table');
      expect(report.prices.single.inputPerM, 2.5);
    });

    test('leaves totals and plans null so their sections stay hidden', () {
      final report = CostsReport.fromJson(_legacyBody());

      expect(report.totals, isNull);
      expect(report.plans, isNull);
      expect(report.lines.first.cachedTokens, 0);
      expect(report.lines.first.onPlan, isFalse);
    });

    test('budget fraction is the share used, clamped to 0 to 1', () {
      expect(
        CostsReport.fromJson(_legacyBody()).budgetFraction,
        closeTo(0.85, 1e-9),
      );

      final over = CostsReport.fromJson({
        ..._legacyBody(),
        'month_usd': 25,
        'budget_usd': 10,
      });
      expect(over.budgetFraction, 1.0);

      final none = CostsReport.fromJson({
        ..._legacyBody(),
        'budget_usd': null,
        'budget_level': null,
      });
      expect(none.budgetUsd, isNull);
      expect(none.budgetFraction, isNull);
      expect(none.budgetLevel, isNull);
    });
  });

  group('CostsReport.fromJson with the 0.70 fields', () {
    test('reads totals in millions and the all-time period', () {
      final report = CostsReport.fromJson(_currentBody());
      final totals = report.totals!;

      expect(totals.month!.inputM, 2.4);
      expect(totals.month!.cachedM, 0.4);
      expect(totals.month!.outputM, 0.35);
      expect(totals.month!.usd, 8.5);
      expect(totals.allTime!.usd, 41.25);
    });

    test('reads plans with their saving', () {
      final plans = CostsReport.fromJson(_currentBody()).plans!;

      expect(plans, hasLength(1));
      expect(plans.single.provider, 'planco');
      expect(plans.single.monthlyUsd, 20);
      expect(plans.single.apiEquivalentUsd, 35.5);
      expect(plans.single.savingUsd, 15.5);
    });

    test('a plan line has no price and reads as on a plan', () {
      final report = CostsReport.fromJson(_currentBody());
      final planLine = report.lines.last;

      expect(planLine.onPlan, isTrue);
      expect(planLine.usd, isNull);
      expect(report.lines.first.cachedTokens, 400000);
      expect(report.lines.first.onPlan, isFalse);
    });

    test('a plan flag counts as on a plan, an empty name does not', () {
      expect(CostLine.fromJson(const {'plan': true}).onPlan, isTrue);
      expect(CostLine.fromJson(const {'plan': '  '}).onPlan, isFalse);
      expect(CostLine.fromJson(const {'plan': false}).onPlan, isFalse);
    });

    test(
      'an empty plans list is kept, so the screen can choose to hide it',
      () {
        final report = CostsReport.fromJson({
          ..._currentBody(),
          'plans': <Object>[],
        });
        expect(report.plans, isEmpty);
      },
    );
  });

  group('CostsReport.fromJson defends against bad values', () {
    test('wrong types fall back to defaults without throwing', () {
      final report = CostsReport.fromJson({
        'month_usd': 'lots',
        'month_unpriced_tokens': 'many',
        'budget_usd': 'ten',
        'budget_level': 'eighty',
        'month_lines': 'nope',
        'prices': 3,
        'totals': 'nope',
        'plans': 'nope',
      });

      expect(report.monthUsd, 0);
      expect(report.monthUnpricedTokens, 0);
      expect(report.budgetUsd, isNull);
      expect(report.budgetLevel, isNull);
      expect(report.lines, isEmpty);
      expect(report.prices, isEmpty);
      expect(report.totals, isNull);
      expect(report.plans, isNull, reason: 'a non-list plans value is absent');
    });

    test('rows that are not objects are skipped', () {
      final report = CostsReport.fromJson({
        'month_lines': [
          'junk',
          7,
          {'model': 'synthetic-model', 'prompt_tokens': '12'},
        ],
        'plans': [
          null,
          {'provider': 'planco', 'monthly_usd': 'x'},
        ],
      });

      expect(report.lines.single.model, 'synthetic-model');
      expect(
        report.lines.single.promptTokens,
        12,
        reason: 'numeric strings count',
      );
      expect(report.plans!.single.monthlyUsd, isNull, reason: 'no plan price');
      expect(report.plans!.single.savingUsd, isNull);
    });

    test('a plan without a price keeps its API total and unpriced tokens', () {
      final plan = PlanCost.fromJson(const {
        'provider': 'planco',
        'monthly_usd': null,
        'api_equivalent_usd': 4.2,
        'unpriced_tokens': 1500000,
        'saving_usd': null,
      });

      expect(plan.monthlyUsd, isNull);
      expect(plan.apiEquivalentUsd, 4.2);
      expect(plan.unpricedTokens, 1500000);
      expect(plan.savingUsd, isNull);
    });

    test('reads the API equivalent per line and per plan period', () {
      final body = _currentBody();
      (body['month_lines'] as List)[1]['api_equivalent_usd'] = 3.75;
      (body['totals'] as Map<String, dynamic>)['month'] = {
        'input_m': 2.4,
        'cached_m': 0.4,
        'output_m': 0.35,
        'usd': 8.5,
        'plan_api_equivalent_usd': 3.75,
      };
      final report = CostsReport.fromJson(body);

      expect(report.lines.last.apiEquivalentUsd, 3.75);
      expect(report.lines.first.apiEquivalentUsd, isNull);
      expect(report.totals!.month!.planApiEquivalentUsd, 3.75);
      expect(report.totals!.allTime!.planApiEquivalentUsd, 0);
    });
  });

  group('plansBody', () {
    test('builds the PUT body, with null clearing a price', () {
      expect(plansBody({'planco': 20, 'otherco': null}), {
        'plans': [
          {'provider': 'planco', 'monthly_usd': 20},
          {'provider': 'otherco', 'monthly_usd': null},
        ],
      });
    });
  });

  group('money and token text', () {
    test('usdText keeps two decimals and a sign', () {
      expect(usdText(3.21236), r'$3.21');
      expect(usdText(0), r'$0.00');
      expect(usdText(-4.5), r'-$4.50');
      expect(usdText(double.nan), r'$0.00');
    });

    test('millionsText shows tokens in millions with two decimals', () {
      expect(millionsText(1234567), '1.23 M');
      expect(millionsText(0), '0.00 M');
      expect(millionsText(2000000), '2.00 M');
    });
  });
}
