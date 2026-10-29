import 'dart:convert';

import 'package:http/http.dart' as http;

import 'models.dart';
import 'validation.dart';

const _maxResponseBytes = 2 * 1024 * 1024;

enum ApiErrorKind {
  validation,
  http,
  transport,
  invalidResponse,
  tooLarge,
}

class ApiException implements Exception {
  ApiException(this.kind, {this.statusCode, this.detail});

  final ApiErrorKind kind;
  final int? statusCode;
  final String? detail;

  bool get unknownOutcome {
    if (kind == ApiErrorKind.validation) {
      return false;
    }
    if (kind == ApiErrorKind.http && statusCode != null) {
      final c = statusCode!;
      if (c >= 400 && c <= 407) {
        return false;
      }
      if (c >= 409 && c <= 499) {
        return false;
      }
    }
    return true;
  }

  String messageForUser() {
    switch (kind) {
      case ApiErrorKind.validation:
        return detail ?? 'Invalid input.';
      case ApiErrorKind.http:
        final c = statusCode ?? 0;
        if (c == 401) {
          return 'Unauthorized (401). Disconnect and check your API token.';
        }
        if (c == 403) {
          return 'Forbidden (403). Server policy or data-egress approval blocks this request.';
        }
        if (c == 429) {
          return 'Rate limited (429). Wait at least one minute, then retry.';
        }
        if (c >= 300 && c < 400) {
          return 'Server redirected the request. Redirects are disabled; check the server URL.';
        }
        return 'Server returned HTTP $c.';
      case ApiErrorKind.transport:
        return 'Connection failed or timed out. Check the server URL, TLS certificate and tunnel.';
      case ApiErrorKind.invalidResponse:
        return 'Unexpected API response. Check server version and compatibility.';
      case ApiErrorKind.tooLarge:
        return 'Server response exceeds the 2 MiB safety limit.';
    }
  }

  @override
  String toString() => messageForUser();
}

/// Owner API client. Token is held in memory only; never logged.
class ApiClient {
  ApiClient._(this._base, this._token, this._http);

  final String _base;
  final String _token;
  final http.Client _http;

  static ApiClient connect(String endpointRaw, String token) {
    final base = validateEndpoint(endpointRaw);
    if (!validToken(token)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'API token must contain 32–512 visible ASCII characters, without spaces.',
      );
    }
    final client = http.Client();
    return ApiClient._(base, token, client);
  }

  void close() {
    _http.close();
  }

  Uri _uri(List<String> segments, [Map<String, String>? query]) {
    final path = segments.join('/');
    var uri = Uri.parse('$_base/$path');
    if (query != null && query.isNotEmpty) {
      uri = uri.replace(queryParameters: query);
    }
    return uri;
  }

  Future<Map<String, dynamic>> _request(
    String method,
    List<String> segments, {
    Map<String, String>? query,
    Map<String, dynamic>? body,
  }) async {
    final uri = _uri(segments, query);
    late final http.Response response;
    try {
      final headers = {
        'Authorization': 'Bearer $_token',
        'Accept': 'application/json',
        if (body != null) 'Content-Type': 'application/json',
      };
      switch (method) {
        case 'GET':
          response = await _http
              .get(uri, headers: headers)
              .timeout(const Duration(seconds: 20));
        case 'POST':
          response = await _http
              .post(uri, headers: headers, body: jsonEncode(body))
              .timeout(const Duration(seconds: 20));
        case 'PUT':
          response = await _http
              .put(uri, headers: headers, body: jsonEncode(body))
              .timeout(const Duration(seconds: 20));
        default:
          throw ApiException(ApiErrorKind.transport);
      }
    } on ApiException {
      rethrow;
    } catch (_) {
      throw ApiException(ApiErrorKind.transport);
    }

    if (response.statusCode >= 300 && response.statusCode < 400) {
      throw ApiException(ApiErrorKind.http, statusCode: response.statusCode);
    }
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw ApiException(ApiErrorKind.http, statusCode: response.statusCode);
    }

    final bytes = response.bodyBytes;
    if (bytes.length > _maxResponseBytes) {
      throw ApiException(ApiErrorKind.tooLarge);
    }

    try {
      final decoded = jsonDecode(utf8.decode(bytes));
      if (decoded is! Map<String, dynamic>) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      return decoded;
    } catch (e) {
      if (e is ApiException) {
        rethrow;
      }
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  void _checkId(String id) {
    if (!validId(id)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'IDs must be 1–128 ASCII letters, digits, underscores, colons or hyphens.',
      );
    }
  }

  TaskInfo _checkTask(TaskInfo task) {
    if (!validId(task.id)) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return task;
  }

  Future<HealthInfo> health() async {
    final json = await _request('GET', ['v1', 'health']);
    return HealthInfo.fromJson(json);
  }

  Future<Catalog> models(String conversationId) async {
    _checkId(conversationId);
    final json = await _request(
      'GET',
      ['v1', 'models'],
      query: {'conversation_id': conversationId},
    );
    return Catalog.fromJson(json);
  }

  Future<List<ChatMessage>> history(String conversationId) async {
    _checkId(conversationId);
    final json = await _request(
      'GET',
      ['v1', 'conversations', conversationId, 'messages'],
      query: {'limit': '100'},
    );
    final list = json['messages'] as List<dynamic>? ?? [];
    if (list.length > 100) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return list
        .map((e) => ChatMessage.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<Selection> selectModel(String conversationId, Selection selection) async {
    _checkId(conversationId);
    if (!selection.isAuto) {
      if (selection.provider.isEmpty ||
          selection.provider.length > 128 ||
          selection.model.isEmpty ||
          selection.model.length > 256) {
        throw ApiException(
          ApiErrorKind.validation,
          detail: 'Choose a provider and model from the server catalog.',
        );
      }
    }
    final json = await _request(
      'PUT',
      ['v1', 'conversations', conversationId, 'model'],
      body: selection.toJson(),
    );
    return Selection.fromJson(json['selection'] as Map<String, dynamic>);
  }

  Future<TaskInfo> submit(String conversationId, String text) async {
    _checkId(conversationId);
    if (!validText(text)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Enter 1–8,000 characters of message text.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'messages'],
      body: {'conversation_id': conversationId, 'text': text},
    );
    return _checkTask(
      TaskInfo.fromJson(json['task'] as Map<String, dynamic>),
    );
  }

  Future<TaskInfo> task(String id) async {
    _checkId(id);
    final json = await _request('GET', ['v1', 'tasks', id]);
    final task = _checkTask(
      TaskInfo.fromJson(json['task'] as Map<String, dynamic>),
    );
    if (task.id != id) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return task;
  }

  Future<List<ApprovalItem>> approvals() async {
    final json = await _request('GET', ['v1', 'approvals']);
    final list = json['approvals'] as List<dynamic>? ?? [];
    if (list.length > 100) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    final items = list
        .map((e) => ApprovalItem.fromJson(e as Map<String, dynamic>))
        .toList();
    for (final a in items) {
      if (!validId(a.id) || !validId(a.actionId)) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      if (a.tool.isEmpty ||
          a.tool.length > 128 ||
          a.summary.runes.length > 4000 ||
          a.expiresAt.length > 64) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
    }
    return items;
  }

  Future<void> decideApproval(String id, ApprovalDecision decision) async {
    _checkId(id);
    await _request(
      'POST',
      ['v1', 'approvals', id, 'decide'],
      body: {'decision': decision.apiValue},
    );
  }
}
