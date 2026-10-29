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
