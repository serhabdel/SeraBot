import 'dart:convert';
import 'dart:math';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/auth.dart';

void main() {
  group('PKCE', () {
    test('S256 challenge matches RFC 7636 vector', () {
      expect(
        s256Challenge('dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk'),
        'E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM',
      );
    });

    test('verifier is 43 unpadded base64url chars and unique', () {
      final a = randomUrlToken();
      final b = randomUrlToken();
      expect(a, matches(RegExp(r'^[A-Za-z0-9_-]{43}$')));
      expect(a, isNot(b));
      expect(randomUrlToken(Random(1)), randomUrlToken(Random(1)));
    });
  });

  group('state check and callback parsing', () {
    test('returns the handoff when state matches', () {
      expect(handoffFromCallback('serabot://auth?handoff=abc&state=S1', 'S1'), 'abc');
    });

    test('rejects a wrong or missing state', () {
      for (final url in [
        'serabot://auth?handoff=abc&state=other',
        'serabot://auth?handoff=abc',
      ]) {
        expect(
          () => handoffFromCallback(url, 'S1'),
          throwsA(isA<SignInException>().having((e) => e.code, 'code', 'state')),
        );
      }
    });

    test('rejects a foreign scheme', () {
      expect(
        () => handoffFromCallback('https://evil.example/?handoff=a&state=S1', 'S1'),
        throwsA(isA<SignInException>()),
      );
    });

    test('surfaces the error code after verifying state', () {
      expect(
        () => handoffFromCallback('serabot://auth?error=denied&state=S1', 'S1'),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'denied')),
      );
      expect(
        () => handoffFromCallback('serabot://auth?error=denied&state=bad', 'S1'),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'state')),
      );
    });
  });

  group('error code mapping', () {
    test('every contract code has a distinct friendly message', () {
      const codes = [
        'denied',
        'invite_required',
        'invite_invalid',
        'disabled',
        'max_users',
        'server',
      ];
      final messages = codes.map(signInErrorMessage).toSet();
      expect(messages.length, codes.length);
      expect(signInErrorMessage('invite_required'), contains('invite'));
      expect(signInErrorMessage('unknown_code'), signInErrorMessage('server'));
    });
  });

  group('sign-in flow', () {
    test('sends contract fields, checks state, exchanges the handoff', () async {
      final requests = <String, Map<String, dynamic>>{};
      final client = MockClient((req) async {
        requests[req.url.path] = jsonDecode(req.body) as Map<String, dynamic>;
        if (req.url.path == '/v1/auth/login/start') {
          return http.Response(
            jsonEncode({'url': 'https://login.example.com/authorize'}),
            200,
          );
        }
        return http.Response(
          jsonEncode({
            'token': 't' * 43,
            'expires_at': '2030-01-01T00:00:00Z',
            'user': {
              'id': 'u1',
              'email': 'a@example.com',
              'display_name': 'Ada',
              'role': 'member',
            },
          }),
          200,
        );
      });
      final api = AuthApi(httpClient: client);
      final result = await api.signIn(
        'https://example.com',
        (url) async {
          expect(url, 'https://login.example.com/authorize');
          final start = requests['/v1/auth/login/start']!;
          return 'serabot://auth?handoff=H1&state=${start['client_state']}';
        },
        inviteCode: ' inv-1 ',
      );
      final start = requests['/v1/auth/login/start']!;
      expect(start['provider'], 'microsoft');
      expect(start['return_to'], 'serabot://auth');
      expect(start['invite_code'], 'inv-1');
      final handoff = requests['/v1/auth/handoff']!;
      expect(handoff['handoff'], 'H1');
      expect(s256Challenge(handoff['verifier'] as String), start['handoff_challenge']);
      expect(result.user.label, 'Ada');
      expect(result.token, 't' * 43);
    });

    test('a closed sign-in page is a cancel, a missing browser is its own error', () async {
      MockClient start() => MockClient((req) async => http.Response(
            jsonEncode({'url': 'https://login.example.com/authorize'}),
            200,
          ));
      Future<void> run(Object error) => AuthApi(httpClient: start()).signIn(
            'https://example.com',
            (url) async => throw error,
          );
      await expectLater(
        run(PlatformException(code: 'CANCELED')),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'cancelled')),
      );
      await expectLater(
        run(PlatformException(code: 'ActivityNotFound')),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'browser')),
      );
      await expectLater(
        run(StateError('x')),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'cancelled')),
      );
      expect(signInErrorMessage('browser'), contains('browser'));
    });

    test('a state mismatch never reaches the handoff endpoint', () async {
      var handoffCalled = false;
      final client = MockClient((req) async {
        if (req.url.path == '/v1/auth/handoff') {
          handoffCalled = true;
        }
        return http.Response(jsonEncode({'url': 'https://login.example.com/a'}), 200);
      });
      await expectLater(
        AuthApi(httpClient: client).signIn(
          'https://example.com',
          (_) async => 'serabot://auth?handoff=H&state=wrong',
        ),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'state')),
      );
      expect(handoffCalled, isFalse);
    });

    test('a 400 on handoff maps to the handoff error', () async {
      String? state;
      final client = MockClient((req) async {
        if (req.url.path == '/v1/auth/login/start') {
          state = (jsonDecode(req.body) as Map<String, dynamic>)['client_state'] as String;
          return http.Response(jsonEncode({'url': 'https://login.example.com/a'}), 200);
        }
        return http.Response(jsonEncode({'error': 'invalid handoff'}), 400);
      });
      await expectLater(
        AuthApi(httpClient: client).signIn(
          'https://example.com',
          (_) async => 'serabot://auth?handoff=H&state=$state',
        ),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'handoff')),
      );
    });
    test('an unsafe authorize URL is refused', () async {
      final client = MockClient(
        (_) async => http.Response(jsonEncode({'url': 'http://evil.example/a'}), 200),
      );
      await expectLater(
        AuthApi(httpClient: client).signIn('https://example.com', (_) async => ''),
        throwsA(isA<SignInException>().having((e) => e.code, 'code', 'server')),
      );
    });
  });

  group('401 handling', () {
    test('invokes onUnauthorized and reports 401', () async {
      var called = 0;
      final client = ApiClient.connect(
        'https://example.com',
        'k' * 43,
        httpClient: MockClient((_) async => http.Response('{}', 401)),
        onUnauthorized: () => called++,
      );
      await expectLater(
        client.me(),
        throwsA(isA<ApiException>().having((e) => e.statusCode, 'status', 401)),
      );
      expect(called, 1);
    });

    test('other errors do not invoke onUnauthorized', () async {
      var called = 0;
      final client = ApiClient.connect(
        'https://example.com',
        'k' * 43,
        httpClient: MockClient((_) async => http.Response('{}', 500)),
        onUnauthorized: () => called++,
      );
      await expectLater(client.me(), throwsA(isA<ApiException>()));
      expect(called, 0);
    });
  });
}
