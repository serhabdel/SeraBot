import 'package:material_ui/material_ui.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/status.dart';

void main() {
  test('flags map to levels, at-limit wins', () {
    expect(statusLevelFromFlags(nearLimit: false, atLimit: false), StatusLevel.ok);
    expect(statusLevelFromFlags(nearLimit: true, atLimit: false), StatusLevel.near);
    expect(statusLevelFromFlags(nearLimit: true, atLimit: true), StatusLevel.atLimit);
    expect(statusLevelFromFlags(nearLimit: false, atLimit: true), StatusLevel.atLimit);
  });

  test('every level has its own word and icon', () {
    final words = StatusLevel.values.map(statusLevelLabel).toSet();
    final icons = StatusLevel.values.map(statusLevelIcon).toSet();
    expect(words, {'OK', 'Near limit', 'At limit'});
    expect(icons.length, StatusLevel.values.length);
  });

  test('quota percent is clamped and safe', () {
    expect(quotaPercent(0, 50), 0);
    expect(quotaPercent(25, 50), 50);
    expect(quotaPercent(60, 50), 100);
    expect(quotaPercent(-5, 50), 0);
    expect(quotaPercent(5, 0), 0);
  });

  for (final brightness in Brightness.values) {
    testWidgets('chip shows icon and word in ${brightness.name} theme', (tester) async {
      for (final level in StatusLevel.values) {
        await tester.pumpWidget(MaterialApp(
          theme: ThemeData(brightness: brightness),
          home: Scaffold(body: Center(child: StatusChip(level: level))),
        ));
        expect(find.text(statusLevelLabel(level)), findsOneWidget);
        expect(find.byIcon(statusLevelIcon(level)), findsOneWidget);
      }
    });
  }
}
