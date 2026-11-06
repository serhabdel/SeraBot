import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;

import 'api_client.dart';
import 'auth.dart';
import 'models.dart';
import 'validation.dart';

/// Signed-in session. The token lives in the OS secret store; this object
/// holds the live client and the signed-in user. Any 401 signs out locally.
class OwnerSession extends ChangeNotifier {
  OwnerSession({TokenStore? store, AuthApi? authApi, this.httpClient})
      : _store = store ?? SecureTokenStore(),
        _authApi = authApi ?? AuthApi();

  final TokenStore _store;
  final AuthApi _authApi;

  /// Test hook: shared HTTP client for authenticated calls.
  final http.Client? httpClient;

  ApiClient? _client;
  String? _endpoint;
  UserInfo? _user;
  String _conversationId = 'android-owner';
  String? _serverVersion;
  String? _statusMessage;
  bool _busy = false;

  bool get connected => _client != null;
  String? get endpoint => _endpoint;
  UserInfo? get user => _user;
  String get conversationId => _conversationId;
  String? get serverVersion => _serverVersion;
  String? get statusMessage => _statusMessage;
  bool get busy => _busy;

  ApiClient get client {
    final c = _client;
    if (c == null) {
      throw StateError('not connected');
    }
    return c;
  }

  void setConversationId(String id) {
    _conversationId = id;
    notifyListeners();
  }

  /// Reuse a stored token if the server still accepts it. Returns whether the
  /// session is now signed in. Never throws.
  Future<bool> restore() async {
    final saved = await _store.read();
    if (saved == null) {
      return false;
    }
    _setBusy(true);
    try {
      await _attach(saved.endpoint, saved.token);
      return true;
    } catch (_) {
      // Unauthorized clears the token itself; other failures keep it so a
      // temporary outage does not sign the user out.
      return false;
    } finally {
      _setBusy(false);
    }
  }

  /// Full Microsoft sign-in per the contract. Throws [SignInException].
  Future<void> signIn(
    String endpointRaw,
    AuthLauncher launch, {
    String? inviteCode,
  }) async {
    _setBusy(true);
    _statusMessage = null;
    try {
      final endpoint = validateEndpoint(endpointRaw);
      final result = await _authApi.signIn(
        endpoint,
        launch,
        inviteCode: inviteCode,
      );
      await _store.write(endpoint, result.token);
      _detach();
      _install(endpoint, result.token, result.user);
    } on ValidationException {
      throw const SignInException('server');
    } finally {
      _setBusy(false);
    }
  }

  Future<void> _attach(String endpoint, String token) async {
    _detach();
    final client = _newClient(endpoint, token);
    try {
      final health = await client.health();
      if (!health.ok) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      final user = await client.me();
      _client = client;
      _endpoint = validateEndpoint(endpoint);
      _user = user;
      _serverVersion = health.version;
      _statusMessage = 'Signed in';
      notifyListeners();
    } catch (_) {
      client.close();
      rethrow;
    }
  }

  void _install(String endpoint, String token, UserInfo user) {
    _client = _newClient(endpoint, token);
    _endpoint = endpoint;
    _user = user;
    _statusMessage = 'Signed in';
    notifyListeners();
    // Best effort; the version label is cosmetic.
    _client!.health().then((h) {
      _serverVersion = h.version;
      notifyListeners();
    }).catchError((_) {});
  }

  ApiClient _newClient(String endpoint, String token) {
    return ApiClient.connect(
      endpoint,
      token,
      httpClient: httpClient,
      onUnauthorized: _onUnauthorized,
    );
  }

  /// A 401 anywhere: drop the token and fall back to the sign-in screen.
  void _onUnauthorized() {
    _store.clear();
    _detach();
    _statusMessage = 'Your session has expired. Sign in again.';
    notifyListeners();
  }

  /// Revoke on the server (best effort), then delete the stored token.
  Future<void> signOut() async {
    final c = _client;
    try {
      await c?.logout();
    } catch (_) {}
    await _store.clear();
    _detach();
    notifyListeners();
  }

  void _detach() {
    _client?.close();
    _client = null;
    _endpoint = null;
    _user = null;
    _serverVersion = null;
    _statusMessage = null;
  }

  @override
  void dispose() {
    _detach();
    _authApi.close();
    super.dispose();
  }

  void setStatus(String? message) {
    _statusMessage = message;
    notifyListeners();
  }

  void _setBusy(bool value) {
    _busy = value;
    notifyListeners();
  }

  Future<T> run<T>(Future<T> Function(ApiClient client) action) async {
    _setBusy(true);
    try {
      return await action(client);
    } finally {
      _setBusy(false);
    }
  }
}
