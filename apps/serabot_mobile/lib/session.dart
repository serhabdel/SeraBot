import 'package:flutter/foundation.dart';

import 'api_client.dart';
import 'validation.dart';

/// In-memory session. Disconnect clears token, client, and cached UI data.
class OwnerSession extends ChangeNotifier {
  ApiClient? _client;
  String? _endpoint;
  String _conversationId = 'android-owner';
  String? _serverVersion;
  String? _statusMessage;
  bool _busy = false;

  bool get connected => _client != null;
  String? get endpoint => _endpoint;
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

  Future<void> connect(String endpointRaw, String token) async {
    _setBusy(true);
    _statusMessage = null;
    try {
      disconnect(notify: false);
      final client = ApiClient.connect(endpointRaw, token);
      final health = await client.health();
      if (!health.ok) {
        client.close();
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      _client = client;
      _endpoint = validateEndpoint(endpointRaw);
      _serverVersion = health.version;
      _statusMessage = 'Connected';
    } finally {
      _setBusy(false);
    }
  }

  void disconnect({bool notify = true}) {
    _client?.close();
    _client = null;
    _endpoint = null;
    _serverVersion = null;
    _statusMessage = null;
    if (notify) {
      notifyListeners();
    }
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
