import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/auth.dart';
import 'package:serabot_mobile/models.dart';
import 'package:serabot_mobile/session.dart';

/// A store that refuses to write until [allow] is set.
class FlakyStore implements TokenStore {
  bool allow = false;
  String? token;

  @override
  Future<({String endpoint, String token})?> read() async => null;

  @override
  Future<void> write(String endpoint, String token) async {
    if (!allow) {
      throw const PlatformException(code: 'store-refused');
    }
    this.token = token;
  }

  @override
  Future<void> clear() async => token = null;
}

class FakeAuthApi extends AuthApi {
  @override
  Future<SignInResult> signIn(
    String endpointRaw,
    AuthLauncher launch, {
    String? inviteCode,
  }) async =>
      SignInResult(
        't' * 43,
        const UserInfo(id: 'u1', email: 'a@example.com', displayName: 'Ada', role: 'member'),
      );
}

void main() {
  OwnerSession build(FlakyStore store) => OwnerSession(
        store: store,
        authApi: FakeAuthApi(),
        httpClient: MockClient((_) async => http.Response(
              jsonEncode({'ok': true, 'version': '0.31.0'}),
              200,
            )),
      );

  test('a refused save keeps the session and offers a retry', () async {
    final store = FlakyStore();
    final session = build(store);
    await session.signIn('https://example.com', (_) async => '');
    expect(session.connected, isTrue);
    expect(session.saveFailed, isTrue);
    expect(store.token, isNull);

    store.allow = true;
    expect(await session.retrySave(), isTrue);
    expect(session.saveFailed, isFalse);
    expect(store.token, 't' * 43);
    session.dispose();
  });

  test('a retry that fails again keeps the warning', () async {
    final store = FlakyStore();
    final session = build(store);
    await session.signIn('https://example.com', (_) async => '');
    expect(await session.retrySave(), isFalse);
    expect(session.saveFailed, isTrue);
    session.dispose();
  });

  test('signing out clears a pending save', () async {
    final store = FlakyStore();
    final session = build(store);
    await session.signIn('https://example.com', (_) async => '');
    await session.signOut();
    expect(session.saveFailed, isFalse);
    expect(session.connected, isFalse);
    session.dispose();
  });
}
