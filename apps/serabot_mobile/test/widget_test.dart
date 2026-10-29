import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/main.dart';
import 'package:serabot_mobile/screens/fleet_tab.dart';
import 'package:serabot_mobile/session.dart';

void main() {
  testWidgets('shows connect screen', (tester) async {
    await tester.pumpWidget(const SeraBotMobileApp());
    expect(find.text('Connect'), findsOneWidget);
  });

  testWidgets('fleet tab shows specialist, skill, and conversation forms', (
    tester,
  ) async {
    final session = OwnerSession();
    addTearDown(session.dispose);
    await tester.binding.setSurfaceSize(const Size(800, 5200));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: FleetTab(session: session, onUseInChat: (_) {}),
        ),
      ),
    );
    expect(find.text('New specialist'), findsOneWidget);
    expect(find.text('New skill'), findsOneWidget);
    expect(find.text('New conversation'), findsOneWidget);
    expect(find.text('Create disabled bot'), findsOneWidget);
    expect(find.text('Create skill'), findsOneWidget);
    expect(find.text('Refresh skills'), findsOneWidget);
    expect(find.text('Create conversation'), findsOneWidget);
  });
}
