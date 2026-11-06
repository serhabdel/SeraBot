import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/main.dart';
import 'package:serabot_mobile/auth.dart';
import 'package:serabot_mobile/screens/fleet_tab.dart';
import 'package:serabot_mobile/session.dart';

class MemoryStore implements TokenStore {
  String? endpoint;
  String? token;

  @override
  Future<({String endpoint, String token})?> read() async =>
      token == null ? null : (endpoint: endpoint!, token: token!);

  @override
  Future<void> write(String endpoint, String token) async {
    this.endpoint = endpoint;
    this.token = token;
  }

  @override
  Future<void> clear() async => token = null;
}

OwnerSession _session(TokenStore store, http.Client client) => OwnerSession(
      store: store,
      authApi: AuthApi(httpClient: client),
      httpClient: client,
    );

void main() {
  testWidgets('shows Microsoft sign-in and no token entry', (tester) async {
    final session = _session(
      MemoryStore(),
      MockClient((_) async => http.Response('{}', 200)),
    );
    await tester.pumpWidget(SeraBotMobileApp(session: session));
    await tester.pumpAndSettle();
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
    expect(find.text('Server URL'), findsOneWidget);
    expect(find.textContaining('token'), findsNothing);
    expect(find.text('Invite code'), findsNothing);
    await tester.tap(find.text('I have an invite code'));
    await tester.pump();
    expect(find.text('Invite code'), findsOneWidget);
  });

  testWidgets('stored token that gets 401 falls back to sign-in', (tester) async {
    final store = MemoryStore()
      ..endpoint = 'https://example.com'
      ..token = 'z' * 43;
    final session = _session(
      store,
      MockClient((_) async => http.Response('{}', 401)),
    );
    await tester.pumpWidget(SeraBotMobileApp(session: session));
    await tester.pumpAndSettle();
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
    expect(store.token, isNull);
  });

  testWidgets('stored valid token shows the user and signs out', (tester) async {
    final store = MemoryStore()
      ..endpoint = 'https://example.com'
      ..token = 'z' * 43;
    final paths = <String>[];
    final session = _session(
      store,
      MockClient((req) async {
        paths.add('${req.method} ${req.url.path}');
        if (req.url.path == '/v1/health') {
          return http.Response(jsonEncode({'ok': true, 'version': '1'}), 200);
        }
        if (req.url.path == '/v1/me') {
          return http.Response(
            jsonEncode({
              'id': 'u1',
              'email': 'a@example.com',
              'display_name': 'Ada',
              'role': 'member',
            }),
            200,
          );
        }
        if (req.url.path == '/v1/auth/logout') {
          return http.Response('{}', 200);
        }
        return http.Response('{}', 500);
      }),
    );
    await tester.binding.setSurfaceSize(const Size(800, 1600));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(SeraBotMobileApp(session: session));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.text('Ada'), findsOneWidget);
    await tester.tap(find.text('Sign out'));
    await tester.pumpAndSettle();
    expect(paths, contains('POST /v1/auth/logout'));
    expect(store.token, isNull);
    expect(find.text('Sign in with Microsoft'), findsOneWidget);
    // Let the tabs' rate-limit delays finish.
    await tester.pump(const Duration(seconds: 4));
  });

  testWidgets('fleet tab shows specialist, skill, and conversation forms', (
    tester,
  ) async {
    final session = OwnerSession(store: MemoryStore());
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
