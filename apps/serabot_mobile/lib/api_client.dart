import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:http/http.dart' as http;

import 'alerts_models.dart';
import 'bot_library_models.dart';
import 'chat_models.dart';
import 'costs_models.dart';
import 'jobs_models.dart';
import 'mail_models.dart';
import 'models.dart';
import 'usage_insights_models.dart';
import 'validation.dart';
import 'workflows_models.dart';

const _maxResponseBytes = 2 * 1024 * 1024;

enum ApiErrorKind { validation, http, transport, invalidResponse, tooLarge }

class ApiException implements Exception {
  ApiException(this.kind, {this.statusCode, this.detail, this.retryAfter});

  final ApiErrorKind kind;
  final int? statusCode;
  final String? detail;

  /// The server's `Retry-After` on a 429 or 503, when it sent one.
  final Duration? retryAfter;

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
        final d = detail;
        if (d != null && d.isNotEmpty && c != 401 && c != 403 && c != 429) {
          return d;
        }
        if (c == 401) {
          return 'Your session has expired. Sign in again.';
        }
        if (c == 403) {
          return 'Forbidden (403). Server policy or data-egress approval blocks this request.';
        }
        if (c == 429) {
          final wait = retryAfter;
          if (wait != null) {
            return 'Rate limited (429). Retry in ${max(1, wait.inSeconds)} seconds.';
          }
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

/// The server's own `{"error": "..."}` sentence for client errors that are
/// safe to show (bounded, no control characters). Null for anything else.
String? serverErrorText(int status, List<int> body) {
  if (!const {400, 404, 409, 413, 422, 503}.contains(status) ||
      body.length > 8192) {
    return null;
  }
  try {
    final v = jsonDecode(utf8.decode(body));
    if (v is Map<String, dynamic> && v['error'] is String) {
      final s = (v['error'] as String).trim();
      if (s.isNotEmpty &&
          s.runes.length <= 512 &&
          !s.runes.any((r) => r <= 0x1f || (r >= 0x7f && r <= 0x9f))) {
        return s;
      }
    }
  } catch (_) {}
  return null;
}

/// Pauses before the second and third try of a read.
const defaultGetRetryDelays = [
  Duration(milliseconds: 400),
  Duration(milliseconds: 1200),
];

/// A `Retry-After` longer than this is shown to the user, not waited out.
const _maxAutoRetryWait = Duration(seconds: 5);

/// Pause before a 401 is asked once more, ahead of signing the user out.
const _reverifyDelay = Duration(milliseconds: 300);

/// No bytes (data or keep-alive comment) for this long means the event
/// stream is half-open. The server sends a comment every 10 seconds.
const eventIdleTimeout = Duration(seconds: 40);

/// [base] spread by up to 20% either way so clients that lost the server
/// together do not return together.
Duration jittered(Duration base, [double? unit]) {
  final u = (unit ?? Random().nextDouble()).clamp(0.0, 1.0);
  return Duration(
    microseconds: (base.inMicroseconds * (0.8 + 0.4 * u)).round(),
  );
}

/// Seconds from a `Retry-After` header, or null when absent or not a number.
Duration? parseRetryAfter(String? value) {
  final seconds = int.tryParse((value ?? '').trim());
  if (seconds == null || seconds < 0 || seconds > 3600) {
    return null;
  }
  return Duration(seconds: seconds);
}

/// Owner API client. Token is held in memory only; never logged.
class ApiClient {
  ApiClient._(
    this._base,
    this._token,
    this._http,
    this._onUnauthorized,
    this._getRetryDelays,
  );

  final String _base;
  final String _token;
  final http.Client _http;
  final void Function()? _onUnauthorized;
  final List<Duration> _getRetryDelays;

  /// [onUnauthorized] runs on every HTTP 401 so the session can drop the
  /// stored token and return to the sign-in screen. [httpClient] is for tests.
  static ApiClient connect(
    String endpointRaw,
    String token, {
    http.Client? httpClient,
    void Function()? onUnauthorized,
    List<Duration> getRetryDelays = defaultGetRetryDelays,
  }) {
    final base = validateEndpoint(endpointRaw);
    if (!validToken(token)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'API token must contain 32–512 visible ASCII characters, without spaces.',
      );
    }
    return ApiClient._(
      base,
      token,
      httpClient ?? http.Client(),
      onUnauthorized,
      getRetryDelays,
    );
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

  /// One authenticated request. Redirects are never followed: the bearer
  /// token must not travel to another URL.
  ///
  /// A read (GET) is tried again after a dropped connection, a 502/503/504, or
  /// a 429 whose `Retry-After` is short. Writes are never repeated: they may
  /// have reached the server.
  Future<http.Response> _send(
    String method,
    Uri uri, {
    List<int>? bodyBytes,
    Map<String, String>? extraHeaders,
    String accept = 'application/json',
    Duration timeout = const Duration(seconds: 20),
  }) async {
    final retries = method == 'GET' ? _getRetryDelays : const <Duration>[];
    var attempt = 0;
    while (true) {
      final http.Response response;
      try {
        final req = http.Request(method, uri)
          ..followRedirects = false
          ..headers.addAll({
            'Authorization': 'Bearer $_token',
            'Accept': accept,
            'X-SeraBot-Client': 'mobile',
            ...?extraHeaders,
          });
        if (bodyBytes != null) {
          req.bodyBytes = bodyBytes;
        }
        final streamed = await _http.send(req).timeout(timeout);
        response = await http.Response.fromStream(streamed).timeout(timeout);
      } catch (_) {
        if (attempt >= retries.length) {
          throw ApiException(ApiErrorKind.transport);
        }
        await Future<void>.delayed(jittered(retries[attempt++]));
        continue;
      }
      final code = response.statusCode;
      if (attempt < retries.length &&
          (code == 429 || code == 502 || code == 503 || code == 504)) {
        final hinted = parseRetryAfter(response.headers['retry-after']);
        if (hinted == null || hinted <= _maxAutoRetryWait) {
          await Future<void>.delayed(jittered(hinted ?? retries[attempt]));
          attempt++;
          continue;
        }
      }
      return response;
    }
  }

  Future<Map<String, dynamic>> _request(
    String method,
    List<String> segments, {
    Map<String, String>? query,
    Map<String, dynamic>? body,
  }) async {
    final uri = _uri(segments, query);
    Future<http.Response> send() => _send(
      method,
      uri,
      bodyBytes: body == null ? null : utf8.encode(jsonEncode(body)),
      extraHeaders: {if (body != null) 'Content-Type': 'application/json'},
    );
    var response = await send();
    if (response.statusCode == 401) {
      // The server rejects before running a handler, so asking again is safe
      // for every method. One proxy or auth hiccup must not sign the user out.
      await Future<void>.delayed(_reverifyDelay);
      response = await send();
    }
    if (response.statusCode == 401) {
      _onUnauthorized?.call();
      throw ApiException(ApiErrorKind.http, statusCode: 401);
    }
    if (response.statusCode >= 300 && response.statusCode < 400) {
      throw ApiException(ApiErrorKind.http, statusCode: response.statusCode);
    }
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw ApiException(
        ApiErrorKind.http,
        statusCode: response.statusCode,
        detail: serverErrorText(response.statusCode, response.bodyBytes),
        retryAfter: parseRetryAfter(response.headers['retry-after']),
      );
    }

    final bytes = response.bodyBytes;
    if (bytes.length > _maxResponseBytes) {
      throw ApiException(ApiErrorKind.tooLarge);
    }
    if (bytes.isEmpty) {
      return <String, dynamic>{};
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
        detail: 'IDs must be 1–128 ASCII letters, digits, underscores, colons or hyphens.',
      );
    }
  }

  TaskInfo _checkTask(TaskInfo task) {
    if (!validId(task.id)) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return task;
  }

  /// The signed-in user.
  Future<UserInfo> me() async {
    final json = await _request('GET', ['v1', 'me']);
    final raw = json['user'] is Map<String, dynamic> ? json['user'] : json;
    return UserInfo.fromJson(raw as Map<String, dynamic>);
  }

  /// Revoke the session on the server. Callers delete the local token
  /// regardless of the outcome.
  Future<void> logout() async {
    await _request('POST', ['v1', 'auth', 'logout'], body: const {});
  }

  Future<HealthInfo> health() async {
    final json = await _request('GET', ['v1', 'health']);
    return HealthInfo.fromJson(json);
  }

  /// Voice, speech, and OCR readiness for this server.
  Future<VoiceSettings> voiceSettings() async {
    final json = await _request('GET', ['v1', 'settings', 'voice']);
    return VoiceSettings.fromJson(json);
  }

  /// Save the voice/OCR settings. Admin only; a member gets 403.
  Future<void> saveVoiceSettings({
    String? speechModel,
    String? voiceId,
    String? replyMode,
    bool? ocrEnabled,
    String? ocrModel,
  }) async {
    if (replyMode != null && replyMode != 'text' && replyMode != 'voice') {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Reply mode must be text or voice.',
      );
    }
    await _request(
      'PUT',
      ['v1', 'settings', 'voice'],
      body: {
        'speech_model': ?speechModel,
        'voice_id': ?voiceId,
        'reply_mode': ?replyMode,
        'ocr_enabled': ?ocrEnabled,
        'ocr_model': ?ocrModel,
      },
    );
  }

  /// The reply mode selected for one conversation.
  Future<ReplyMode> replyMode(String conversationId) async {
    _checkId(conversationId);
    final json = await _request('GET', [
      'v1',
      'conversations',
      conversationId,
      'reply-mode',
    ]);
    return ReplyMode.fromJson(json);
  }

  /// Switch one conversation between text and voice replies.
  Future<ReplyMode> setReplyMode(String conversationId, String mode) async {
    _checkId(conversationId);
    if (mode != 'text' && mode != 'voice') {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Reply mode must be text or voice.',
      );
    }
    final json = await _request(
      'PUT',
      ['v1', 'conversations', conversationId, 'reply-mode'],
      body: {'mode': mode},
    );
    return ReplyMode.fromJson(json);
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

  /// The conversation's goal and checklist, or null on an older server.
  Future<ChatPlan?> conversationPlan(String conversationId) async {
    _checkId(conversationId);
    try {
      final json = await _request('GET', [
        'v1',
        'conversations',
        conversationId,
        'plan',
      ]);
      final raw = json['plan'];
      if (raw is Map<String, dynamic>) {
        return ChatPlan.parse(raw);
      }
      return null;
    } on ApiException catch (e) {
      if (e.kind == ApiErrorKind.http && e.statusCode == 404) {
        return null;
      }
      rethrow;
    }
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

  Future<Selection> selectModel(
    String conversationId,
    Selection selection,
  ) async {
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
    final json = await _request('PUT', [
      'v1',
      'conversations',
      conversationId,
      'model',
    ], body: selection.toJson());
    return Selection.fromJson(json['selection'] as Map<String, dynamic>);
  }

  Future<TaskInfo> submit(
    String conversationId,
    String text, {
    List<String> attachments = const [],
  }) async {
    _checkId(conversationId);
    if (attachments.length > maxAttachments || !attachments.every(validId)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Attach at most 6 files.',
      );
    }
    final textOk =
        validText(text) || (attachments.isNotEmpty && text.trim().isEmpty);
    if (!textOk) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Enter 1–8,000 characters of message text.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'messages'],
      body: {
        'conversation_id': conversationId,
        'text': text,
        if (attachments.isNotEmpty) 'attachments': attachments,
      },
    );
    return _checkTask(TaskInfo.fromJson(json['task'] as Map<String, dynamic>));
  }

  /// All of this user's conversations, in the server's order (the same
  /// `GET /v1/conversations` the desktop rail reads), following the cursor.
  Future<List<ConversationSummary>> conversations({
    bool includeArchived = false,
    int pageSize = 100,
    int maxPages = 10,
  }) async {
    final out = <ConversationSummary>[];
    String? cursor;
    for (var page = 0; page < maxPages; page++) {
      final json = await _request(
        'GET',
        ['v1', 'conversations'],
        query: {
          'limit': '$pageSize',
          'include_archived': includeArchived ? 'true' : 'false',
          'cursor': ?cursor,
        },
      );
      final ConversationPage parsed;
      try {
        parsed = ConversationPage.fromJson(json, pageSize);
      } catch (_) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      for (final c in parsed.conversations) {
        if (!validId(c.id)) {
          throw ApiException(ApiErrorKind.invalidResponse);
        }
      }
      out.addAll(parsed.conversations);
      cursor = parsed.nextCursor;
      if (cursor == null || cursor.isEmpty) {
        break;
      }
    }
    return out;
  }

  /// Full-text message search (`GET /v1/conversations/search?q=...&bot=...&
  /// limit=...`, 0.74.2): UID-scoped, searches message content and titles,
  /// deduplicates by conversation (newest match first) and returns bounded
  /// ~160-char snippets. Queries under 2 characters return no hits without
  /// hitting the server, mirroring the desktop; an older server answering
  /// 404 also yields an empty hit list instead of an error.
  Future<List<ConversationSearchHit>> searchConversations(
    String query, {
    String? bot,
    int limit = 15,
  }) async {
    final q = query.trim();
    if (q.runes.length < 2 || q.runes.length > 200) {
      return const [];
    }
    final queryParams = <String, String>{
      'q': q,
      'limit': '${limit.clamp(1, 50)}',
      if (bot != null && bot.trim().isNotEmpty) 'bot': bot.trim(),
    };
    final json = await _requestOptional(
      'GET',
      ['v1', 'conversations', 'search'],
      query: queryParams,
    );
    if (json == null) {
      return const [];
    }
    try {
      return parseConversationSearchHits(json);
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// A new node in the tree. [kind] direct/room sit under a section or group.
  Future<ConversationSummary> createChat({
    required ConversationKind kind,
    required String slug,
    String? parentId,
    String? title,
  }) async {
    if (!validSlug(slug)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Slug must be a lowercase slug of at most 32 characters.',
      );
    }
    final t = title?.trim();
    if (t != null && t.isNotEmpty && (t.runes.length > 120)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Names must be 1-120 characters.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'conversations'],
      body: {
        'kind': kind.apiValue,
        'slug': slug,
        'parent_id': ?parentId,
        if (t != null && t.isNotEmpty) 'title': t,
      },
    );
    return ConversationSummary.fromJson(
      json['conversation'] as Map<String, dynamic>,
    );
  }

  /// `POST /v1/groups`: a group chat with its members in one step.
  Future<ConversationSummary> createGroupChat(
    String name,
    List<String> members,
  ) async {
    final n = name.trim();
    if (n.isEmpty || n.runes.length > 60) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Give the group a name of 1 to 60 characters.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'groups'],
      body: {'name': n, 'members': members},
    );
    return ConversationSummary.fromJson(json['group'] as Map<String, dynamic>);
  }

  Future<void> renameConversation(String id, String title) async {
    _checkId(id);
    final t = title.trim();
    if (t.isEmpty || t.runes.length > 120) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Title must be 1-120 characters.',
      );
    }
    await _request('PATCH', ['v1', 'conversations', id], body: {'title': t});
  }

  Future<void> archiveConversation(String id) async {
    _checkId(id);
    await _request('DELETE', ['v1', 'conversations', id]);
  }

  /// Undo [archiveConversation].
  Future<void> unarchiveConversation(String id) async {
    _checkId(id);
    await _request('POST', [
      'v1',
      'conversations',
      id,
      'unarchive',
    ], body: const {});
  }

  /// Stop the running turn of one conversation.
  Future<void> cancelTurn(String id) async {
    _checkId(id);
    await _request('POST', [
      'v1',
      'conversations',
      id,
      'cancel',
    ], body: const {});
  }

  Future<List<LibraryEntry>> library(String conversationId) async {
    _checkId(conversationId);
    final json = await _request('GET', [
      'v1',
      'conversations',
      conversationId,
      'library',
    ]);
    final list = json['artifacts'];
    if (list is! List || list.length > 100) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    try {
      final items = list
          .map((e) => LibraryEntry.fromJson(e as Map<String, dynamic>))
          .toList();
      if (items.any((e) => !validHash(e.hash))) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      return items;
    } on ApiException {
      rethrow;
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// Upload one chat attachment (raw bytes, `X-Filename`), same route as the
  /// desktop. Returns the attachment to reference in the next message.
  Future<AttachmentInfo> uploadAttachment(
    String conversationId,
    String filename,
    List<int> bytes,
  ) async {
    _checkId(conversationId);
    if (bytes.isEmpty) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'That file is empty.',
      );
    }
    if (bytes.length > maxAttachmentBytes) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Files must be 25 MB or smaller.',
      );
    }
    if (!validAttachmentName(filename)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'That file type is not supported. Use images, PDF, Office, text, markdown or CSV files.',
      );
    }
    final response = await _send(
      'POST',
      _uri(['v1', 'conversations', conversationId, 'attachments']),
      bodyBytes: bytes,
      extraHeaders: {
        'X-Filename': filename.trim(),
        'Content-Type': 'application/octet-stream',
      },
      timeout: const Duration(minutes: 2),
    );
    final json = _decode(response);
    final raw = json['attachment'];
    if (raw is! Map<String, dynamic>) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    final info = AttachmentInfo.fromJson(raw);
    if (!validId(info.id)) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return info;
  }

  /// Download a produced file by content hash (cap 100 MiB).
  Future<List<int>> artifactBytes(String hash) async {
    if (!validHash(hash)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Invalid artifact hash.',
      );
    }
    return _fetch(['v1', 'artifacts', hash]);
  }

  /// Server-rendered preview of one artifact (0.28.0):
  /// `GET /v1/artifacts/{hash}/preview` -> `{content_type, text?, image_b64?}`.
  /// Null when the server has no preview for it (404) or the route is not
  /// shipped yet; the caller falls back to [artifactBytes].
  Future<ArtifactPreview?> artifactPreview(String hash) async {
    if (!validHash(hash)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Invalid artifact hash.',
      );
    }
    final response = await _send(
      'GET',
      _uri(['v1', 'artifacts', hash, 'preview']),
    );
    if (response.statusCode == 404) {
      return null;
    }
    final json = _decode(response);
    return ArtifactPreview.fromJson(json);
  }

  /// Download one of the user's own attachments.
  Future<List<int>> attachmentBytes(String id) async {
    _checkId(id);
    return _fetch(['v1', 'attachments', id]);
  }

  Future<List<int>> _fetch(List<String> segments) async {
    final response = await _send(
      'GET',
      _uri(segments),
      accept: '*/*',
      timeout: const Duration(minutes: 2),
    );
    _checkStatus(response);
    if (response.bodyBytes.length > 100 * 1024 * 1024) {
      throw ApiException(ApiErrorKind.tooLarge);
    }
    return response.bodyBytes;
  }

  /// Bot or group picture; null when none is set or the image is not a PNG.
  Future<List<int>?> avatarBytes(String kind, String id) async {
    _checkId(id);
    final response = await _send(
      'GET',
      _uri(['v1', kind, id, 'avatar']),
      accept: 'image/*',
    );
    if (response.statusCode == 404) {
      return null;
    }
    _checkStatus(response);
    final b = response.bodyBytes;
    if (b.length > 2 * 1024 * 1024 || b.length < 8) {
      return null;
    }
    const png = [0x89, 0x50, 0x4e, 0x47];
    for (var i = 0; i < 4; i++) {
      if (b[i] != png[i]) {
        return null;
      }
    }
    return b;
  }

  Future<void> setBotAvatar(String id, List<int> png) async {
    _checkId(id);
    if (png.isEmpty || png.length > 2 * 1024 * 1024) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Pictures must be a PNG of at most 2 MB.',
      );
    }
    final response = await _send(
      'PUT',
      _uri(['v1', 'bots', id, 'avatar']),
      bodyBytes: png,
      extraHeaders: {'Content-Type': 'application/octet-stream'},
      timeout: const Duration(seconds: 60),
    );
    _checkStatus(response);
  }

  /// Upload or replace a channel/conversation avatar (`PUT /v1/conversations/{id}/avatar`).
  Future<void> setConversationAvatar(String id, List<int> bytes) async {
    _checkId(id);
    if (bytes.isEmpty || bytes.length > 2 * 1024 * 1024) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Pictures must be at most 2 MB.',
      );
    }
    final response = await _send(
      'PUT',
      _uri(['v1', 'conversations', id, 'avatar']),
      bodyBytes: bytes,
      extraHeaders: {'Content-Type': 'application/octet-stream'},
      timeout: const Duration(seconds: 60),
    );
    _checkStatus(response);
  }

  /// Speech to text for the composer mic button. The transcript is model
  /// output: the user reviews it before sending.
  Future<String> transcribe(String filename, List<int> audio) async {
    if (audio.isEmpty) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'The recording is empty.',
      );
    }
    if (audio.length > maxAudioBytes) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'The recording is larger than the 20 MiB limit.',
      );
    }
    final response = await _send(
      'POST',
      _uri(['v1', 'audio', 'transcribe']),
      bodyBytes: audio,
      extraHeaders: {
        'X-Audio-Filename': filename,
        'Content-Type': 'application/octet-stream',
      },
      timeout: const Duration(minutes: 2),
    );
    return Transcription.fromJson(_decode(response)).text;
  }

  Future<List<BotSummary>> bots() async {
    final json = await _request('GET', ['v1', 'bots']);
    final list = json['bots'];
    if (list is! List || list.length > 200) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    try {
      return list
          .map((e) => BotSummary.fromJson(e as Map<String, dynamic>))
          .toList();
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  Future<BotDetail> botDetail(String id) async {
    _checkId(id);
    final json = await _request('GET', ['v1', 'bots', id]);
    try {
      final d = BotDetail.fromJson(json);
      if (d.id != id) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      return d;
    } on ApiException {
      rethrow;
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// `PUT /v1/bots/{id}`: name, description, instructions, model, abilities.
  Future<void> updateBot(String id, Map<String, dynamic> body) async {
    _checkId(id);
    if (body.isEmpty) {
      return;
    }
    await _request('PUT', ['v1', 'bots', id], body: body);
  }

  /// `POST /v1/bots` with a name; the server derives the id.
  Future<String> createNamedBot(String name) async {
    final n = name.trim();
    if (n.isEmpty || n.runes.length > 60) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Give the bot a name of 1 to 60 characters.',
      );
    }
    final json = await _request('POST', ['v1', 'bots'], body: {'name': n});
    final bot = json['bot'];
    final id = bot is Map<String, dynamic> ? bot['id'] : null;
    if (id is! String || !validId(id)) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return id;
  }

  Future<UsageReport> usage({int days = 30}) async {
    final json = await _request(
      'GET',
      ['v1', 'usage'],
      query: {'days': '$days'},
    );
    return UsageReport.fromJson(json);
  }

  /// Channels, the PC tunnel, model speed and, for the admin, server load
  /// (`GET /v1/usage/insights`, 0.67). Counts only, never message text.
  Future<UsageInsights> usageInsights({int days = 7}) async {
    if (days < 1 || days > 90) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Usage days must be 1\u201390.',
      );
    }
    final json = await _request(
      'GET',
      ['v1', 'usage', 'insights'],
      query: {'days': '$days'},
    );
    return UsageInsights.fromJson(json);
  }

  /// Alerts and delivery settings for the caller (`GET /v1/alerts`, 0.68).
  Future<AlertsView> alerts() async {
    final json = await _request('GET', ['v1', 'alerts']);
    return AlertsView.fromJson(json);
  }

  /// Saves delivery choices and returns the refreshed view (`PUT /v1/alerts`).
  Future<AlertsView> saveAlerts(AlertsUpdate update) async {
    final json = await _request('PUT', ['v1', 'alerts'], body: update.toJson());
    return AlertsView.fromJson(json);
  }

  /// Sends the caller their weekly digest now (`POST /v1/alerts/digest`).
  Future<DigestPreview> sendDigestNow() async {
    final json = await _request('POST', ['v1', 'alerts', 'digest']);
    return DigestPreview.fromJson(json);
  }

  /// The caller's brief as text, or null when this server has no brief route
  /// (`POST /v1/brief/preview`, 0.70).
  Future<String?> briefPreview() async {
    final json = await _requestOptional('POST', ['v1', 'brief', 'preview']);
    if (json == null) {
      return null;
    }
    return alertText(json['text']);
  }

  /// This month's estimated model cost and the budget (`GET /v1/usage/costs`).
  Future<CostsReport> usageCosts() async {
    final json = await _request('GET', ['v1', 'usage', 'costs']);
    return CostsReport.fromJson(json);
  }

  /// Sets or clears (null) the monthly budget and returns the refreshed costs
  /// (`PUT /v1/usage/budget`).
  Future<CostsReport> setMonthlyBudget(double? usd) async {
    final json = await _request(
      'PUT',
      ['v1', 'usage', 'budget'],
      body: {'monthly_usd': usd},
    );
    return CostsReport.fromJson(json);
  }

  /// Sets plan prices per provider, null to clear one, and returns the
  /// refreshed costs (`PUT /v1/usage/plans`, 0.70).
  Future<CostsReport> setPlanPrices(
    Map<String, double?> monthlyByProvider,
  ) async {
    final json = await _request('PUT', [
      'v1',
      'usage',
      'plans',
    ], body: plansBody(monthlyByProvider));
    return CostsReport.fromJson(json);
  }

  /// The job search profile (`GET /v1/jobs/profile`, 0.70). A 404 means the
  /// server predates job search; the screen handles it.
  Future<JobProfile> jobsProfile() async {
    final json = await _request('GET', ['v1', 'jobs', 'profile']);
    return JobProfile.fromJson(json);
  }

  /// Watched job sources, read-only (`GET /v1/jobs/sources`, 0.70).
  Future<List<JobSource>> jobsSources() async {
    final json = await _request('GET', ['v1', 'jobs', 'sources']);
    return jobSourcesFrom(json);
  }

  /// Tracked applications (`GET /v1/jobs/applications`, 0.70).
  Future<List<JobApplication>> jobsApplications() async {
    final json = await _request('GET', ['v1', 'jobs', 'applications']);
    return jobApplicationsFrom(json);
  }

  /// Checks the sources now and returns the matches (`POST /v1/jobs/watch`).
  Future<JobWatchResult> jobsWatch() async {
    final json = await _request('POST', ['v1', 'jobs', 'watch']);
    return JobWatchResult.fromJson(json);
  }

  /// Files in a bot's chats: the ones its bots made and the member's uploads
  /// (`GET /v1/bots/{bot}/library`, 0.63). Bad rows are skipped.
  Future<List<BotLibraryItem>> botLibrary(String botId) async {
    _checkId(botId);
    final json = await _request('GET', ['v1', 'bots', botId, 'library']);
    return parseBotLibrary(json);
  }

  /// Stored `turn.memory` events of one chat, oldest first (0.61), so a reply's
  /// memories show after the chat is reopened. Empty on servers without it.
  Future<List<ServerEvent>> conversationMemoryEvents(
    String conversationId, {
    int limit = 100,
  }) async {
    _checkId(conversationId);
    final json = await _requestOptional(
      'GET',
      ['v1', 'conversations', conversationId, 'activity'],
      query: {'limit': '$limit'},
    );
    final rows = json?['steps'];
    if (rows is! List) {
      return const [];
    }
    return [
      for (final row in rows.whereType<Map<String, dynamic>>())
        if (row['kind'] == 'turn.memory' &&
            row['payload'] is Map<String, dynamic>)
          ServerEvent(0, 'turn.memory', row['payload'] as Map<String, dynamic>),
    ];
  }

  Future<List<ConnectorStatus>> connectors() async {
    final json = await _request('GET', ['v1', 'connectors']);
    final list = json['connectors'];
    if (list is! List || list.length > 64) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    try {
      return list
          .map((e) => ConnectorStatus.fromJson(e as Map<String, dynamic>))
          .toList();
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// Per-user SMTP/IMAP mail status (`GET /v1/mail`, 0.74.1–0.74.2). Never
  /// returns the password. A 404 means the server predates mail, which
  /// reads as "not connected" instead of an error.
  Future<MailStatus> mailStatus() async {
    final json = await _requestOptional('GET', ['v1', 'mail']);
    if (json == null) {
      return const MailStatus(connected: false);
    }
    try {
      return MailStatus.fromJson(json);
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// Saves per-user SMTP/IMAP credentials and runs the live SMTP + IMAP
  /// login tests (`POST /v1/mail/connect`). Validated locally first, so a
  /// plain-text remote host never leaves the phone.
  Future<MailConnectResponse> mailConnect(MailConnectRequest req) async {
    final problem = validateMailConnect(req);
    if (problem != null) {
      throw ApiException(ApiErrorKind.validation, detail: problem);
    }
    final json = await _request(
      'POST',
      ['v1', 'mail', 'connect'],
      body: req.toJson(),
    );
    try {
      return MailConnectResponse.fromJson(json);
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// Removes the saved per-user SMTP/IMAP credentials (`DELETE /v1/mail`).
  Future<MailDisconnectResponse> mailDisconnect() async {
    final json = await _request('DELETE', ['v1', 'mail']);
    try {
      return MailDisconnectResponse.fromJson(json);
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// Server events after [after] (`GET /v1/events`, Server-Sent Events). With
  /// no cursor the stream starts at the newest event (`after=latest`): current
  /// state comes from the normal loads, not from replaying retained history.
  ///
  /// [onOpen] runs once the server answers 200, before any event, so callers
  /// can show the link as up even on a quiet stream. The stream errors with a
  /// transport failure when nothing, not even a keep-alive, arrives for
  /// [idleTimeout]; the caller reconnects from the last id it saw. A 401 is
  /// asked once more, then signs the session out like any other call.
  Stream<ServerEvent> events({
    int after = 0,
    void Function()? onOpen,
    Duration idleTimeout = eventIdleTimeout,
  }) async* {
    Future<http.StreamedResponse> open() async {
      final req =
          http.Request(
              'GET',
              _uri(
                ['v1', 'events'],
                {'after': after > 0 ? '$after' : 'latest', 'limit': '100'},
              ),
            )
            ..followRedirects = false
            ..headers.addAll({
              'Authorization': 'Bearer $_token',
              'Accept': 'text/event-stream',
            });
      try {
        return await _http.send(req).timeout(const Duration(seconds: 20));
      } catch (_) {
        throw ApiException(ApiErrorKind.transport);
      }
    }

    var res = await open();
    if (res.statusCode == 401) {
      await res.stream.drain<void>().catchError((_) {});
      await Future<void>.delayed(_reverifyDelay);
      res = await open();
    }
    if (res.statusCode == 401) {
      _onUnauthorized?.call();
      throw ApiException(ApiErrorKind.http, statusCode: 401);
    }
    if (res.statusCode != 200) {
      throw ApiException(
        ApiErrorKind.http,
        statusCode: res.statusCode,
        retryAfter: parseRetryAfter(res.headers['retry-after']),
      );
    }
    onOpen?.call();
    final parser = SseParser();
    try {
      final watched = res.stream.timeout(
        idleTimeout,
        onTimeout: (sink) {
          sink.addError(ApiException(ApiErrorKind.transport));
          sink.close();
        },
      );
      await for (final chunk in watched.transform(utf8.decoder)) {
        for (final frame in parser.feed(chunk)) {
          final e = ServerEvent.fromFrame(frame);
          if (e != null) {
            yield e;
          }
        }
      }
    } on http.ClientException {
      throw ApiException(ApiErrorKind.transport);
    }
  }

  void _checkStatus(http.Response response) {
    if (response.statusCode == 401) {
      _onUnauthorized?.call();
      throw ApiException(ApiErrorKind.http, statusCode: 401);
    }
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw ApiException(
        ApiErrorKind.http,
        statusCode: response.statusCode,
        detail: serverErrorText(response.statusCode, response.bodyBytes),
      );
    }
  }

  Map<String, dynamic> _decode(http.Response response) {
    _checkStatus(response);
    final bytes = response.bodyBytes;
    if (bytes.length > _maxResponseBytes) {
      throw ApiException(ApiErrorKind.tooLarge);
    }
    if (bytes.isEmpty) {
      return <String, dynamic>{};
    }
    try {
      final v = jsonDecode(utf8.decode(bytes));
      if (v is Map<String, dynamic>) {
        return v;
      }
    } catch (_) {}
    throw ApiException(ApiErrorKind.invalidResponse);
  }

  Future<List<ApprovalItem>> approvalsPlaceholder() async => approvals();
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

  /// Personal tasks (`GET /v1/tasks`). Returns `supported: false` on 404.
  Future<TasksResponse> tasks({String? status}) async {
    final query = <String, String>{};
    if (status != null && status.isNotEmpty) {
      query['status'] = status;
    }
    final response = await _send(
      'GET',
      _uri(['v1', 'tasks'], query.isEmpty ? null : query),
    );
    if (response.statusCode == 404) {
      return const TasksResponse(supported: false);
    }
    return TasksResponse.fromJson(_decode(response));
  }

  Future<PersonalTask> createTask({
    required String title,
    String? notes,
    String? status,
    String? due,
  }) async {
    final body = <String, dynamic>{'title': title};
    if (notes != null && notes.isNotEmpty) {
      body['notes'] = notes;
    }
    if (status != null && status.isNotEmpty) {
      body['status'] = status;
    }
    if (due != null && due.isNotEmpty) {
      body['due'] = due;
    }
    final json = await _request('POST', ['v1', 'tasks'], body: body);
    return PersonalTask.fromJson(json['task'] as Map<String, dynamic>);
  }

  Future<PersonalTask> updateTask(
    String id, {
    String? title,
    String? notes,
    String? status,
    String? due,
    bool clearDue = false,
  }) async {
    _checkId(id);
    final body = <String, dynamic>{};
    if (title != null) body['title'] = title;
    if (notes != null) body['notes'] = notes;
    if (status != null) body['status'] = status;
    if (clearDue) {
      body['due'] = null;
    } else if (due != null) {
      body['due'] = due;
    }
    final json = await _request('PATCH', ['v1', 'tasks', id], body: body);
    return PersonalTask.fromJson(json['task'] as Map<String, dynamic>);
  }

  Future<void> deleteTask(String id) async {
    _checkId(id);
    await _request('DELETE', ['v1', 'tasks', id]);
  }

  Future<void> syncTasksRun() async {
    await _request('POST', ['v1', 'tasks', 'sync', 'run']);
  }

  Future<List<CalendarEventItem>> calendarEvents({
    String? from,
    String? to,
  }) async {
    final query = <String, String>{};
    if (from != null && from.isNotEmpty) query['from'] = from;
    if (to != null && to.isNotEmpty) query['to'] = to;
    final response = await _send(
      'GET',
      _uri(['v1', 'tasks', 'calendar'], query.isEmpty ? null : query),
    );
    if (response.statusCode == 404) {
      return const [];
    }
    final json = _decode(response);
    final list = json['events'] as List<dynamic>? ?? const [];
    return list
        .whereType<Map<String, dynamic>>()
        .map(CalendarEventItem.fromJson)
        .toList();
  }

  /// Scaffolds a disabled specialist. An empty [tools] list is sent as-is;
  /// the server then writes an empty tool set rather than its default.
  Future<CreatedBot> createBot(
    String id,
    String description,
    List<String> tools,
    List<String> skills,
  ) async {
    if (!validBotId(id)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Bot id must be a lowercase slug of at most 32 characters, and cannot be main or a Windows device name.',
      );
    }
    if (!validBotDescription(description)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Description must be at most 2,000 characters, without control characters.',
      );
    }
    try {
      validateToolList(tools);
    } on ValidationException catch (e) {
      throw ApiException(ApiErrorKind.validation, detail: e.message);
    }
    if (skills.length > 16) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Enter at most 16 skill ids.',
      );
    }
    final seenSkills = <String>{};
    for (final skill in skills) {
      if (!validSkillId(skill)) {
        throw ApiException(
          ApiErrorKind.validation,
          detail: 'Skill ids are lowercase slugs such as ask.',
        );
      }
      if (!seenSkills.add(skill)) {
        throw ApiException(
          ApiErrorKind.validation,
          detail: 'Each skill id can be listed once.',
        );
      }
    }
    final json = await _request(
      'POST',
      ['v1', 'bots'],
      body: {
        'id': id,
        'description': description,
        'tools': tools,
        'skills': skills,
      },
    );
    return _checkCreatedBot(json, id);
  }

  Future<CreatedConversation> createConversation(
    ConversationKind kind,
    String slug,
    String? parentId,
  ) async {
    if (!validSlug(slug)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Slug must be a lowercase slug of at most 32 characters.',
      );
    }
    final parent = (parentId == null || parentId.isEmpty) ? null : parentId;
    try {
      validateConversationParent(kind, parent);
    } on ValidationException catch (e) {
      throw ApiException(ApiErrorKind.validation, detail: e.message);
    }
    final json = await _request(
      'POST',
      ['v1', 'conversations'],
      body: {'kind': kind.apiValue, 'slug': slug, 'parent_id': ?parent},
    );
    return _checkCreatedConversation(json, kind, slug, parent);
  }

  /// Registers a skill pack on disk. A blank [body] is omitted; the server
  /// writes a stub and does not echo the markdown.
  Future<CreatedSkill> createSkill(
    String id,
    String description,
    String? body,
  ) async {
    if (!validNewSkillId(id)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Skill id must be a lowercase slug of at most 64 characters, and cannot be a Windows device name.',
      );
    }
    final summary = description.trim();
    if (!validSkillDescription(summary)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Description must be one line of at most 500 characters, without quotes or backslashes.',
      );
    }
    final markdown = body?.trim();
    final text = (markdown == null || markdown.isEmpty) ? null : markdown;
    if (text != null && !validSkillBody(text)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Body must be markdown of at most 96,000 bytes, without control characters.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'skills'],
      body: {'id': id, 'description': summary, 'body': ?text},
    );
    return _checkCreatedSkill(json, id);
  }

  Future<Map<String, dynamic>?> _requestOptional(
    String method,
    List<String> segments, {
    Map<String, String>? query,
    Map<String, dynamic>? body,
  }) async {
    try {
      return await _request(method, segments, query: query, body: body);
    } on ApiException catch (e) {
      if (e.kind == ApiErrorKind.http && e.statusCode == 404) {
        return null;
      }
      rethrow;
    }
  }

  /// Routines, monitoring sources and active plans (0.29 Wave A / C5 read-only).
  /// Each leg treats HTTP 404 as unsupported instead of failing the bundle.
  Future<WorkflowsSnapshot> workflowsSnapshot() async {
    var snapshot = const WorkflowsSnapshot();
    final routinesJson = await _requestOptional('GET', ['v1', 'routines']);
    if (routinesJson != null) {
      final rows = parseRoutines(routinesJson);
      if (rows == null) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      snapshot = WorkflowsSnapshot(
        routines: rows,
        sources: snapshot.sources,
        active: snapshot.active,
        routinesSupported: true,
        sourcesSupported: snapshot.sourcesSupported,
        activeSupported: snapshot.activeSupported,
      );
    }
    final sourcesJson = await _requestOptional('GET', [
      'v1',
      'monitoring',
      'sources',
    ]);
    if (sourcesJson != null) {
      final rows = parseSources(sourcesJson);
      if (rows == null) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      snapshot = WorkflowsSnapshot(
        routines: snapshot.routines,
        sources: rows,
        active: snapshot.active,
        routinesSupported: snapshot.routinesSupported,
        sourcesSupported: true,
        activeSupported: snapshot.activeSupported,
      );
    }
    final activeJson = await _requestOptional('GET', [
      'v1',
      'workflows',
      'active',
    ]);
    if (activeJson != null) {
      final rows = parseActiveWorkflows(activeJson);
      if (rows == null) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      snapshot = WorkflowsSnapshot(
        routines: snapshot.routines,
        sources: snapshot.sources,
        active: rows,
        routinesSupported: snapshot.routinesSupported,
        sourcesSupported: snapshot.sourcesSupported,
        activeSupported: true,
      );
    }
    return snapshot;
  }

  /// Redacted tool and approval timeline for one conversation (Wave A5).
  /// Returns null when the server does not expose `/v1/traces` yet.
  Future<ConversationTracePage?> conversationTraces(
    String conversationId, {
    int limit = 100,
  }) async {
    _checkId(conversationId);
    if (limit < 1 || limit > 500) {
      throw ApiException(
        ApiErrorKind.validation,
        detail: 'Trace limit must be 1–500.',
      );
    }
    final json = await _requestOptional(
      'GET',
      ['v1', 'traces'],
      query: {'conversation_id': conversationId, 'limit': '$limit'},
    );
    if (json == null) {
      return null;
    }
    try {
      return parseTraces(json);
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }

  /// On-disk skill summaries. Bodies are not part of this response.
  Future<List<SkillSummary>> listSkills() async {
    final json = await _request('GET', ['v1', 'skills']);
    try {
      final list = json['skills'];
      if (list is! List || list.length > maxSkillList) {
        throw ApiException(ApiErrorKind.invalidResponse);
      }
      final skills = <SkillSummary>[];
      final seen = <String>{};
      for (final entry in list) {
        if (entry is! Map<String, dynamic>) {
          throw ApiException(ApiErrorKind.invalidResponse);
        }
        final skill = SkillSummary.fromJson(entry);
        if (!validSkillId(skill.id) || !seen.add(skill.id)) {
          throw ApiException(ApiErrorKind.invalidResponse);
        }
        if (!_boundedSkillLabel(skill.name) ||
            skill.description.runes.length > 2000 ||
            skill.description.runes.any(_isControlRune)) {
          throw ApiException(ApiErrorKind.invalidResponse);
        }
        final pack = skill.pack;
        if (pack != null && !validSkillId(pack)) {
          throw ApiException(ApiErrorKind.invalidResponse);
        }
        skills.add(skill);
      }
      return skills;
    } on ApiException {
      rethrow;
    } catch (_) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
  }
}

CreatedBot _checkCreatedBot(Map<String, dynamic> json, String id) {
  try {
    final raw = json['bot'];
    if (raw is! Map<String, dynamic>) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    final bot = CreatedBot.fromJson(raw);
    if (bot.id != id ||
        bot.enabled ||
        bot.live ||
        bot.role != 'specialist' ||
        bot.tools.length > 32 ||
        bot.skills.length > 16 ||
        bot.requireApproval.length > 32 ||
        !_safeFleetFile(bot.botFile, id, 'toml') ||
        !_safeFleetFile(bot.promptFile, id, 'txt') ||
        bot.tools.any((tool) => tool == 'delegate' || !_boundedName(tool)) ||
        bot.skills.any((skill) => !validSkillId(skill)) ||
        bot.requireApproval.any(
          (tool) => !_boundedName(tool) || !bot.tools.contains(tool),
        )) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return bot;
  } on ApiException {
    rethrow;
  } catch (_) {
    throw ApiException(ApiErrorKind.invalidResponse);
  }
}

CreatedConversation _checkCreatedConversation(
  Map<String, dynamic> json,
  ConversationKind kind,
  String slug,
  String? parentId,
) {
  try {
    final raw = json['conversation'];
    if (raw is! Map<String, dynamic>) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    final node = CreatedConversation.fromJson(raw);
    final expectedId = conversationNodeId(kind, slug);
    if (expectedId == null ||
        node.id != expectedId ||
        node.kind != kind.apiValue ||
        node.acceptsTurns != kind.acceptsTurns ||
        node.parentId != parentId) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return node;
  } on ApiException {
    rethrow;
  } catch (_) {
    throw ApiException(ApiErrorKind.invalidResponse);
  }
}

bool _boundedName(String value) {
  if (value.isEmpty || value.length > 64) {
    return false;
  }
  return value.codeUnits.every((b) {
    return (b >= 0x30 && b <= 0x39) ||
        (b >= 0x41 && b <= 0x5a) ||
        (b >= 0x61 && b <= 0x7a) ||
        b == 0x5f ||
        b == 0x2d;
  });
}

bool _safeFleetFile(String path, String id, String ext) {
  if (path.length > 256 ||
      path.contains('\\') ||
      path.contains(':') ||
      path.contains('..') ||
      path.startsWith('/') ||
      path.contains('//')) {
    return false;
  }
  for (final b in path.codeUnits) {
    final ok =
        (b >= 0x30 && b <= 0x39) ||
        (b >= 0x41 && b <= 0x5a) ||
        (b >= 0x61 && b <= 0x7a) ||
        b == 0x2f ||
        b == 0x2e ||
        b == 0x5f ||
        b == 0x2d;
    if (!ok) {
      return false;
    }
  }
  final slash = path.lastIndexOf('/');
  if (slash <= 0) {
    return false;
  }
  final dir = path.substring(0, slash);
  final file = path.substring(slash + 1);
  return dir.isNotEmpty && file == '$id.$ext';
}

bool _isControlRune(int r) => r <= 0x1f || (r >= 0x7f && r <= 0x9f);

bool _boundedSkillLabel(String value) {
  if (value.isEmpty || value.runes.length > 128) {
    return false;
  }
  for (final r in value.runes) {
    if (_isControlRune(r) || r == 0x2f || r == 0x5c || r == 0x3a) {
      return false;
    }
  }
  return true;
}

bool _safeSkillFile(String path, String id) {
  final nested = '/$id/SKILL.md';
  final flat = '$id/SKILL.md';
  if (path.length > 256 ||
      path.startsWith('/') ||
      path.contains('\\') ||
      path.contains(':') ||
      path.contains('..') ||
      path.contains('//') ||
      !(path.endsWith(nested) || path == flat)) {
    return false;
  }
  for (final b in path.codeUnits) {
    final ok =
        (b >= 0x30 && b <= 0x39) ||
        (b >= 0x41 && b <= 0x5a) ||
        (b >= 0x61 && b <= 0x7a) ||
        b == 0x2f ||
        b == 0x2e ||
        b == 0x5f ||
        b == 0x2d;
    if (!ok) {
      return false;
    }
  }
  return true;
}

CreatedSkill _checkCreatedSkill(Map<String, dynamic> json, String id) {
  try {
    final raw = json['skill'];
    if (raw is! Map<String, dynamic>) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    final skill = CreatedSkill.fromJson(raw);
    if (skill.id != id ||
        skill.live ||
        !_boundedSkillLabel(skill.name) ||
        skill.description.runes.length > maxSkillDescriptionChars ||
        skill.description.runes.any(_isControlRune) ||
        !_safeSkillFile(skill.file, id)) {
      throw ApiException(ApiErrorKind.invalidResponse);
    }
    return skill;
  } on ApiException {
    rethrow;
  } catch (_) {
    throw ApiException(ApiErrorKind.invalidResponse);
  }
}
