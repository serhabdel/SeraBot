import 'dart:convert';
import 'dart:math';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart' show PlatformException;
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:http/http.dart' as http;

import 'models.dart';
import 'validation.dart';

/// Fixed return target for the mobile sign-in (see native-signin-contract).
const signInReturnTo = 'serabot://auth';
const signInCallbackScheme = 'serabot';

const _maxAuthBytes = 64 * 1024;

String _b64url(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');

/// Base64url of 32 random bytes (43 chars, within the 43-128 contract range).
String randomUrlToken([Random? random]) {
  final rng = random ?? Random.secure();
  return _b64url(List<int>.generate(32, (_) => rng.nextInt(256)));
}

/// `base64url(sha256(verifier))`, unpadded (PKCE S256).
String s256Challenge(String verifier) =>
    _b64url(sha256.convert(ascii.encode(verifier)).bytes);

/// Why a sign-in failed. [code] is the contract's short error code, or
/// `state`, `cancelled`, `handoff`, `network`, `server` for local failures.
class SignInException implements Exception {
  const SignInException(this.code);

  final String code;

  String get message => signInErrorMessage(code);

  @override
  String toString() => message;
}

/// Friendly text for a callback `error` code or local failure code.
String signInErrorMessage(String code) {
  switch (code) {
    case 'denied':
      return 'Sign-in was cancelled or denied.';
    case 'invite_required':
      return 'This account needs an invite code. Enter one below and try again.';
    case 'invite_invalid':
      return 'That invite code is not valid or has already been used.';
    case 'disabled':
      return 'This account is disabled. Contact the administrator.';
    case 'max_users':
      return 'The server has reached its user limit.';
    case 'state':
      return 'Sign-in could not be verified. Please try again.';
    case 'cancelled':
      return 'Sign-in was cancelled.';
    case 'browser':
      return 'The sign-in page could not be opened. Make sure a browser '
          '(such as Chrome) is installed and enabled, then try again.';
    case 'handoff':
      return 'Sign-in expired or was already used. Please try again.';
    case 'network':
      return 'Could not reach the server. Check the server URL and connection.';
    default:
      return 'Sign-in failed on the server. Please try again.';
  }
}

/// Reads the callback URL. Verifies `state` first, then returns the handoff
/// code or throws [SignInException] for an `error` code.
String handoffFromCallback(String callbackUrl, String expectedState) {
  final uri = Uri.tryParse(callbackUrl);
  if (uri == null || uri.scheme != signInCallbackScheme) {
    throw const SignInException('state');
  }
  final params = uri.queryParameters;
  final state = params['state'];
  if (state == null || !_constantTimeEquals(state, expectedState)) {
    throw const SignInException('state');
  }
  final error = params['error'];
  if (error != null && error.isNotEmpty) {
    throw SignInException(error);
  }
  final handoff = params['handoff'];
  if (handoff == null || handoff.isEmpty) {
    throw const SignInException('handoff');
  }
  return handoff;
}

bool _constantTimeEquals(String a, String b) {
  if (a.length != b.length) {
    return false;
  }
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a.codeUnitAt(i) ^ b.codeUnitAt(i);
  }
  return diff == 0;
}

class SignInResult {
  const SignInResult(this.token, this.user);

  final String token;
  final UserInfo user;
}

/// Opens the authorize URL and returns the callback URL (flutter_web_auth_2).
typedef AuthLauncher = Future<String> Function(String url);

/// Pre-session auth calls: `login/start` and `handoff`.
class AuthApi {
  AuthApi({http.Client? httpClient, this._random})
      : _http = httpClient ?? http.Client();

  final http.Client _http;
  final Random? _random;

  Future<SignInResult> signIn(
    String endpointRaw,
    AuthLauncher launch, {
    String? inviteCode,
  }) async {
    final base = validateEndpoint(endpointRaw);
    final invite = inviteCode?.trim();
    if (invite != null &&
        invite.isNotEmpty &&
        (invite.length > 256 ||
            invite.codeUnits.any((u) => u < 0x20 || u == 0x7f))) {
      throw const SignInException('invite_invalid');
    }
    final verifier = randomUrlToken(_random);
    final clientState = randomUrlToken(_random);
    final startBody = {
      'provider': 'microsoft',
      if (invite != null && invite.isNotEmpty) 'invite_code': invite,
      'return_to': signInReturnTo,
      'handoff_challenge': s256Challenge(verifier),
      'client_state': clientState,
    };
    final start = await _post('$base/v1/auth/login/start', startBody);
    final url = start['url'];
    if (url is! String || !_isSafeAuthorizeUrl(url)) {
      throw const SignInException('server');
    }
    final String callback;
    try {
      callback = await launch(url);
    } on PlatformException catch (e) {
      // The user closing the sign-in page is a cancel; any other platform
      // error means no browser could show it.
      throw SignInException(e.code == 'CANCELED' ? 'cancelled' : 'browser');
    } catch (_) {
      throw const SignInException('cancelled');
    }
    final handoff = handoffFromCallback(callback, clientState);
    final done = await _post(
      '$base/v1/auth/handoff',
      {'handoff': handoff, 'verifier': verifier},
      handoffEndpoint: true,
    );
    final token = done['token'];
    final user = done['user'];
    if (token is! String || !validToken(token) || user is! Map<String, dynamic>) {
      throw const SignInException('server');
    }
    try {
      return SignInResult(token, UserInfo.fromJson(user));
    } catch (_) {
      throw const SignInException('server');
    }
  }

  Future<Map<String, dynamic>> _post(
    String url,
    Map<String, dynamic> body, {
    bool handoffEndpoint = false,
  }) async {
    final http.Response response;
    try {
      response = await _http
          .post(
            Uri.parse(url),
            headers: const {
              'Accept': 'application/json',
              'Content-Type': 'application/json',
            },
            body: jsonEncode(body),
          )
          .timeout(const Duration(seconds: 20));
    } catch (_) {
      throw const SignInException('network');
    }
    if (response.statusCode == 400 && handoffEndpoint) {
      throw const SignInException('handoff');
    }
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw SignInException(
        response.statusCode == 400 ? 'invite_invalid' : 'server',
      );
    }
    if (response.bodyBytes.length > _maxAuthBytes) {
      throw const SignInException('server');
    }
    try {
      final decoded = jsonDecode(utf8.decode(response.bodyBytes));
      if (decoded is Map<String, dynamic>) {
        return decoded;
      }
    } catch (_) {}
    throw const SignInException('server');
  }

  void close() => _http.close();
}

/// Only HTTPS, or loopback HTTP for a local development server.
bool _isSafeAuthorizeUrl(String value) {
  final uri = Uri.tryParse(value);
  if (uri == null || !uri.hasAuthority) {
    return false;
  }
  if (uri.scheme == 'https') {
    return true;
  }
  if (uri.scheme != 'http') {
    return false;
  }
  final host = uri.host;
  return host == 'localhost' || host == '127.0.0.1' || host == '::1';
}

/// Where the session token and server URL live between launches.
abstract class TokenStore {
  Future<({String endpoint, String token})?> read();
  Future<void> write(String endpoint, String token);
  Future<void> clear();
}

/// OS secret store: Keychain on iOS, Keystore-backed on Android.
class SecureTokenStore implements TokenStore {
  SecureTokenStore([FlutterSecureStorage? storage])
      : _storage = storage ?? const FlutterSecureStorage();

  static const _tokenKey = 'serabot.session_token';
  static const _endpointKey = 'serabot.endpoint';

  final FlutterSecureStorage _storage;

  @override
  Future<({String endpoint, String token})?> read() async {
    try {
      final token = await _storage.read(key: _tokenKey);
      final endpoint = await _storage.read(key: _endpointKey);
      if (token == null || endpoint == null) {
        return null;
      }
      return (endpoint: endpoint, token: token);
    } catch (_) {
      return null;
    }
  }

  @override
  Future<void> write(String endpoint, String token) async {
    await _storage.write(key: _tokenKey, value: token);
    await _storage.write(key: _endpointKey, value: endpoint);
  }

  @override
  Future<void> clear() async {
    try {
      await _storage.delete(key: _tokenKey);
    } catch (_) {}
  }
}
