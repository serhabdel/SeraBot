import 'dart:convert';

import 'package:http/http.dart' as http;

import 'chat_models.dart';
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

/// Owner API client. Token is held in memory only; never logged.
class ApiClient {
  ApiClient._(this._base, this._token, this._http, this._onUnauthorized);

  final String _base;
  final String _token;
  final http.Client _http;
  final void Function()? _onUnauthorized;

  /// [onUnauthorized] runs on every HTTP 401 so the session can drop the
  /// stored token and return to the sign-in screen. [httpClient] is for tests.
  static ApiClient connect(
    String endpointRaw,
    String token, {
    http.Client? httpClient,
    void Function()? onUnauthorized,
  }) {
    final base = validateEndpoint(endpointRaw);
    if (!validToken(token)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'API token must contain 32–512 visible ASCII characters, without spaces.',
      );
    }
    return ApiClient._(base, token, httpClient ?? http.Client(), onUnauthorized);
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
  Future<http.Response> _send(
    String method,
    Uri uri, {
    List<int>? bodyBytes,
    Map<String, String>? extraHeaders,
    String accept = 'application/json',
    Duration timeout = const Duration(seconds: 20),
  }) async {
    try {
      final req = http.Request(method, uri)
        ..followRedirects = false
        ..headers.addAll({
          'Authorization': 'Bearer $_token',
          'Accept': accept,
          ...?extraHeaders,
        });
      if (bodyBytes != null) {
        req.bodyBytes = bodyBytes;
      }
      final streamed = await _http.send(req).timeout(timeout);
      return await http.Response.fromStream(streamed).timeout(timeout);
    } catch (_) {
      throw ApiException(ApiErrorKind.transport);
    }
  }

  Future<Map<String, dynamic>> _request(
    String method,
    List<String> segments, {
    Map<String, String>? query,
    Map<String, dynamic>? body,
  }) async {
    final uri = _uri(segments, query);
    final response = await _send(
      method,
      uri,
      bodyBytes: body == null ? null : utf8.encode(jsonEncode(body)),
      extraHeaders: {if (body != null) 'Content-Type': 'application/json'},
    );
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
      throw ApiException(ApiErrorKind.validation,
          detail: 'Reply mode must be text or voice.');
    }
    await _request('PUT', ['v1', 'settings', 'voice'], body: {
      'speech_model': ?speechModel,
      'voice_id': ?voiceId,
      'reply_mode': ?replyMode,
      'ocr_enabled': ?ocrEnabled,
      'ocr_model': ?ocrModel,
    });
  }

  /// The reply mode selected for one conversation.
  Future<ReplyMode> replyMode(String conversationId) async {
    _checkId(conversationId);
    final json = await _request(
      'GET',
      ['v1', 'conversations', conversationId, 'reply-mode'],
    );
    return ReplyMode.fromJson(json);
  }

  /// Switch one conversation between text and voice replies.
  Future<ReplyMode> setReplyMode(String conversationId, String mode) async {
    _checkId(conversationId);
    if (mode != 'text' && mode != 'voice') {
      throw ApiException(ApiErrorKind.validation,
          detail: 'Reply mode must be text or voice.');
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
    return _checkTask(
      TaskInfo.fromJson(json['task'] as Map<String, dynamic>),
    );
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

  /// Stop the running turn of one conversation.
  Future<void> cancelTurn(String id) async {
    _checkId(id);
    await _request(
      'POST',
      ['v1', 'conversations', id, 'cancel'],
      body: const {},
    );
  }

  Future<List<LibraryEntry>> library(String conversationId) async {
    _checkId(conversationId);
    final json = await _request(
      'GET',
      ['v1', 'conversations', conversationId, 'library'],
    );
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
      throw ApiException(ApiErrorKind.validation, detail: 'That file is empty.');
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
        detail:
            'That file type is not supported. Use images, PDF, Office, text, markdown or CSV files.',
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

  /// Server events after [after] (`GET /v1/events`, Server-Sent Events). The
  /// stream ends when the connection drops; the caller reconnects from the
  /// last id it saw. A 401 signs the session out like any other call.
  Stream<ServerEvent> events({int after = 0}) async* {
    final req = http.Request(
      'GET',
      _uri(
        ['v1', 'events'],
        {if (after > 0) 'after': '$after', 'limit': '100'},
      ),
    )
      ..followRedirects = false
      ..headers.addAll({
        'Authorization': 'Bearer $_token',
        'Accept': 'text/event-stream',
      });
    final http.StreamedResponse res;
    try {
      res = await _http.send(req).timeout(const Duration(seconds: 20));
    } catch (_) {
      throw ApiException(ApiErrorKind.transport);
    }
    if (res.statusCode == 401) {
      _onUnauthorized?.call();
      throw ApiException(ApiErrorKind.http, statusCode: 401);
    }
    if (res.statusCode != 200) {
      throw ApiException(ApiErrorKind.http, statusCode: res.statusCode);
    }
    final parser = SseParser();
    try {
      await for (final chunk in res.stream.transform(utf8.decoder)) {
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
        detail:
            'Bot id must be a lowercase slug of at most 32 characters, and cannot be main or a Windows device name.',
      );
    }
    if (!validBotDescription(description)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'Description must be at most 2,000 characters, without control characters.',
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
      body: {
        'kind': kind.apiValue,
        'slug': slug,
        'parent_id': ?parent,
      },
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
        detail:
            'Skill id must be a lowercase slug of at most 64 characters, and cannot be a Windows device name.',
      );
    }
    final summary = description.trim();
    if (!validSkillDescription(summary)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'Description must be one line of at most 500 characters, without quotes or backslashes.',
      );
    }
    final markdown = body?.trim();
    final text = (markdown == null || markdown.isEmpty) ? null : markdown;
    if (text != null && !validSkillBody(text)) {
      throw ApiException(
        ApiErrorKind.validation,
        detail:
            'Body must be markdown of at most 96,000 bytes, without control characters.',
      );
    }
    final json = await _request(
      'POST',
      ['v1', 'skills'],
      body: {
        'id': id,
        'description': summary,
        'body': ?text,
      },
    );
    return _checkCreatedSkill(json, id);
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
    final ok = (b >= 0x30 && b <= 0x39) ||
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
    final ok = (b >= 0x30 && b <= 0x39) ||
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
