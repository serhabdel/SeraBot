import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:material_ui/material_ui.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';
import 'package:serabot_mobile/screens/costs_screen.dart';

final _token = 'w' * 43;

http.Response _json(Object body) => http.Response(
  jsonEncode(body),
  200,
  headers: {'content-type': 'application/json'},
);

AppState _app(Future<http.Response> Function(http.Request) handler) {
  final client = ApiClient.connect(
    'https://example.com',
    _token,
    getRetryDelays: const [],
    httpClient: MockClient(handler),
  );
  return AppState(client);
}

/// Synthetic costs with a budget, a priced model, a plan-covered model and a
/// plan comparison (0.70 fields included).
Map<String, dynamic> _costsBody({double? budget = 10}) => {
  'month_usd': 12.5,
  'month_unpriced_tokens': 0,
  'budget_usd': budget,
  // The server's level: 100 once spend reaches the budget, else none here.
  'budget_level': budget == null || budget > 12.5 ? null : 100,
  'month_lines': [
    {
      'provider': 'acme',
      'model': 'synthetic-model',
      'prompt_tokens': 1500000,
      'completion_tokens': 250000,
      'cached_tokens': 400000,
      'usd': 12.5,
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
  'prices': <Object>[],
  'totals': {
    'month': {'input_m': 2.4, 'cached_m': 0.4, 'output_m': 0.35, 'usd': 12.5},
    'all_time': {'input_m': 10, 'cached_m': 1, 'output_m': 2, 'usd': 41.25},
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

/// The list is lazy: rows below the fold are built only once scrolled to. The
/// budget field has its own Scrollable, so take the list's (the first one).
Finder _listScroll() => find
    .descendant(of: find.byType(ListView), matching: find.byType(Scrollable))
    .first;

Future<void> _reveal(WidgetTester tester, Finder finder) {
  return tester.scrollUntilVisible(finder, 200, scrollable: _listScroll());
}

void main() {
  testWidgets('costs screen shows estimate, budget, plans and model rows', (
    tester,
  ) async {
    final app = _app((request) async {
      expect(request.url.path, '/v1/usage/costs');
      return _json(_costsBody());
    });
    await tester.pumpWidget(MaterialApp(home: CostsScreen(app: app)));
    await tester.pumpAndSettle();

    expect(find.text('Estimated this month'), findsOneWidget);
    expect(find.text(r'$12.50'), findsWidgets);
    expect(
      find.textContaining(r'$12.50 of $10.00'),
      findsOneWidget,
      reason: 'budget line reads spent of budget',
    );
    expect(find.textContaining('Over budget.'), findsOneWidget);
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('budget-field')))
          .controller!
          .text,
      '10.00',
    );

    expect(find.text('Tokens and cost'), findsOneWidget);
    expect(
      find.textContaining('This month: in 2.40 M, cached 0.40 M, out 0.35 M'),
      findsOneWidget,
    );

    await _reveal(tester, find.text('Plans vs API'));
    expect(find.text('Plans vs API'), findsOneWidget);
    expect(find.text('planco'), findsOneWidget);
    expect(find.textContaining(r'Plan $20.00 a month'), findsOneWidget);
    expect(find.textContaining(r'Saves $15.50'), findsOneWidget);

    await _reveal(
      tester,
      find.byKey(const Key('cost-line-acme-synthetic-model')),
    );
    final priced = find.byKey(const Key('cost-line-acme-synthetic-model'));
    expect(
      find.descendant(of: priced, matching: find.textContaining('In 1.50 M')),
      findsOneWidget,
    );
    expect(
      find.descendant(of: priced, matching: find.textContaining('Out 0.25 M')),
      findsOneWidget,
    );
    expect(
      find.descendant(of: priced, matching: find.text(r'$12.50')),
      findsOneWidget,
    );

    final planRow = find.byKey(const Key('cost-line-planco-plan-model'));
    await _reveal(tester, planRow);
    expect(
      find.descendant(of: planRow, matching: find.text('plan')),
      findsOneWidget,
      reason: 'a plan row shows "plan" instead of a price',
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('costs screen without 0.70 fields hides the new sections', (
    tester,
  ) async {
    final legacy = Map<String, dynamic>.from(_costsBody())
      ..remove('totals')
      ..remove('plans');
    final app = _app((request) async => _json(legacy));
    await tester.pumpWidget(MaterialApp(home: CostsScreen(app: app)));
    await tester.pumpAndSettle();

    expect(find.text('Tokens and cost'), findsNothing);
    expect(find.text('Plans vs API'), findsNothing);
    await _reveal(tester, find.text('By model this month'));
    expect(find.text('By model this month'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('saving a budget sends the amount and shows the new budget', (
    tester,
  ) async {
    Map<String, dynamic>? sent;
    final app = _app((request) async {
      if (request.method == 'PUT') {
        expect(request.url.path, '/v1/usage/budget');
        sent = jsonDecode(request.body) as Map<String, dynamic>;
        return _json(_costsBody(budget: 50));
      }
      return _json(_costsBody(budget: null));
    });
    await tester.pumpWidget(MaterialApp(home: CostsScreen(app: app)));
    await tester.pumpAndSettle();

    expect(
      find.text('No budget set. Alerts warn at 80% and 100% of it.'),
      findsOneWidget,
    );

    await tester.enterText(find.byKey(const Key('budget-field')), '50');
    await tester.tap(find.byKey(const Key('budget-save')));
    await tester.pumpAndSettle();

    expect(sent, {'monthly_usd': 50.0});
    expect(find.textContaining(r'$12.50 of $50.00'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('a zero budget is refused before any request is sent', (
    tester,
  ) async {
    var puts = 0;
    final app = _app((request) async {
      if (request.method == 'PUT') {
        puts++;
      }
      return _json(_costsBody(budget: null));
    });
    await tester.pumpWidget(MaterialApp(home: CostsScreen(app: app)));
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('budget-field')), '0');
    await tester.tap(find.byKey(const Key('budget-save')));
    await tester.pumpAndSettle();

    expect(puts, 0);
    expect(
      find.text('Enter a monthly budget above \$0, or clear it.'),
      findsOneWidget,
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });
}
