import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/main.dart';

void main() {
  testWidgets('shows connect screen', (tester) async {
    await tester.pumpWidget(const SeraBotMobileApp());
    expect(find.text('Connect'), findsOneWidget);
  });
}
