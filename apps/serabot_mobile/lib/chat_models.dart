import 'dart:convert';

/// Models for the per-user conversation API (`GET /v1/conversations`, bots,
/// library, usage, connectors) and the `GET /v1/events` stream. They mirror
/// the desktop console's `api.rs` so both clients show the same data.

String? _str(Object? v) => v is String ? v : null;
int _int(Object? v) => v is int ? v : (v is num ? v.toInt() : 0);

/// Header-safe upload name: ASCII only (the server wants a plain name), the
/// extension kept, anything else replaced by _.
String safeUploadName(String name) {
  final base = name.split(RegExp(r'[\\/]')).last.trim();
  final b = StringBuffer();
  for (final r in base.runes) {
    final ok = r >= 0x20 && r < 0x7f && !'/\\:"\''.runes.contains(r);
    b.write(ok ? String.fromCharCode(r) : '_');
  }
  var s = b.toString();
  if (s.length > 128) {
    final dot = s.lastIndexOf('.');
    final ext = dot >= 0 && s.length - dot <= 12 ? s.substring(dot) : '';
    s = s.substring(0, 128 - ext.length) + ext;
  }
  return s;
}

class LastMessage {
  const LastMessage({required this.role, required this.text});
  final String role;
  final String text;

  static LastMessage? fromJson(Object? v) {
    if (v is! Map<String, dynamic>) {
      return null;
    }
    return LastMessage(
      role: _str(v['role']) ?? '',
      text: _str(v['text']) ?? '',
    );
  }
}

/// One row of `GET /v1/conversations`.
class ConversationSummary {
  const ConversationSummary({
    required this.id,
    required this.kind,
    this.parentId,
    this.title,
    this.createdAt = '',
    this.updatedAt = '',
    this.lastMessageAt,
    this.archivedAt,
    this.unreadCount = 0,
    this.acceptsTurns = true,
    this.lastMessage,
    this.avatarVersion,
  });

  factory ConversationSummary.fromJson(Map<String, dynamic> json) {
    final id = json['id'];
    final kind = json['kind'];
    if (id is! String || kind is! String) {
      throw const FormatException('conversation id/kind missing');
    }
    return ConversationSummary(
      id: id,
      kind: kind == 'legacy' ? 'direct' : kind,
      parentId: _str(json['parent_id']),
      title: _str(json['title']),
      createdAt: _str(json['created_at']) ?? '',
      updatedAt: _str(json['updated_at']) ?? '',
      lastMessageAt: _str(json['last_message_at']),
      archivedAt: _str(json['archived_at']),
      unreadCount: _int(json['unread_count']),
      acceptsTurns: json['accepts_turns'] as bool? ?? true,
      lastMessage: LastMessage.fromJson(json['last_message']),
      avatarVersion: _str(json['avatar_version']),
    );
  }

  final String id;
  final String kind;
  final String? parentId;
  final String? title;
  final String createdAt;
  final String updatedAt;
  final String? lastMessageAt;
  final String? archivedAt;
  final int unreadCount;
  final bool acceptsTurns;
  final LastMessage? lastMessage;
  final String? avatarVersion;

  /// Same rule as the desktop: the title, else the id.
  String get displayTitle {
    final t = title;
    return (t != null && t.trim().isNotEmpty) ? t : id;
  }

  bool get isSection => kind == 'section';
  bool get isGroup => kind == 'group';
  bool get isRoom => kind == 'room';
  bool get isChat => kind == 'direct' || kind == 'room';
  bool get archived => archivedAt != null;

  ConversationSummary copyWith({int? unreadCount}) => ConversationSummary(
        id: id,
        kind: kind,
        parentId: parentId,
        title: title,
        createdAt: createdAt,
        updatedAt: updatedAt,
        lastMessageAt: lastMessageAt,
        archivedAt: archivedAt,
        unreadCount: unreadCount ?? this.unreadCount,
        acceptsTurns: acceptsTurns,
        lastMessage: lastMessage,
        avatarVersion: avatarVersion,
      );
}

class ConversationPage {
  const ConversationPage(this.conversations, this.nextCursor);
  final List<ConversationSummary> conversations;
  final String? nextCursor;

  factory ConversationPage.fromJson(Map<String, dynamic> json, int limit) {
    final list = json['conversations'];
    if (list is! List || list.length > limit) {
      throw const FormatException('bad conversation page');
    }
    return ConversationPage(
      list
          .map((e) => ConversationSummary.fromJson(e as Map<String, dynamic>))
          .toList(),
      _str(json['next_cursor']),
    );
  }
}

/// A folder in the chat list: a section or group and the chats under it.
class ConversationBucket {
  ConversationBucket(this.folder);
  final ConversationSummary? folder;
  final List<ConversationSummary> chats = [];
  int get unread => chats.fold(0, (a, c) => a + c.unreadCount);
}

/// Arranges the flat server list like the desktop rail: chats without a
/// parent first, then each section/group with its chats. The order the server
/// returned is kept inside every bucket.
List<ConversationBucket> bucketConversations(
  List<ConversationSummary> all, {
  String query = '',
}) {
  final q = query.trim().toLowerCase();
  bool matches(ConversationSummary c) =>
      q.isEmpty ||
      c.displayTitle.toLowerCase().contains(q) ||
      (c.lastMessage?.text.toLowerCase().contains(q) ?? false);

  final folders = {
    for (final c in all)
      if (c.isSection || c.isGroup) c.id: c,
  };
  final byFolder = <String, ConversationBucket>{};
  final loose = ConversationBucket(null);
  for (final c in all) {
    if (!c.isChat || !matches(c)) {
      continue;
    }
    final parent = c.parentId;
    if (parent == null || !folders.containsKey(parent)) {
      loose.chats.add(c);
    } else {
      byFolder
          .putIfAbsent(parent, () => ConversationBucket(folders[parent]!))
          .chats
          .add(c);
    }
  }
  final out = <ConversationBucket>[];
  if (loose.chats.isNotEmpty) {
    out.add(loose);
  }
  for (final c in all) {
    final b = byFolder[c.id];
    if (b != null) {
      out.add(b);
    }
  }
  return out;
}

/// Total unread across the list (chats only).
int totalUnread(List<ConversationSummary> all) =>
    all.where((c) => c.isChat).fold(0, (a, c) => a + c.unreadCount);

class AttachmentInfo {
  const AttachmentInfo({
    required this.id,
    required this.filename,
    this.mime = '',
    this.sizeBytes = 0,
    this.isImage = false,
  });

  factory AttachmentInfo.fromJson(Map<String, dynamic> json) {
    return AttachmentInfo(
      id: json['id'] as String,
      filename: _str(json['filename']) ?? 'file',
      mime: _str(json['mime']) ?? '',
      sizeBytes: _int(json['size_bytes']),
      isImage: json['is_image'] as bool? ?? false,
    );
  }

  final String id;
  final String filename;
  final String mime;
  final int sizeBytes;
  final bool isImage;
}

/// Extensions the server accepts for chat attachments (same as desktop).
const attachmentExtensions = [
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'pdf', 'docx', 'xlsx', 'pptx', //
  'txt', 'md', 'markdown', 'csv',
];
const maxAttachmentBytes = 25 * 1024 * 1024;
const maxAttachments = 6;
const maxAudioBytes = 20 * 1024 * 1024;

bool validAttachmentName(String name) {
  final t = name.trim();
  if (t.isEmpty || t.length > 128) {
    return false;
  }
  for (final r in t.runes) {
    if (r <= 0x1f || (r >= 0x7f && r <= 0x9f) || '/\\:"\''.runes.contains(r)) {
      return false;
    }
  }
  final dot = t.lastIndexOf('.');
  if (dot < 0) {
    return false;
  }
  return attachmentExtensions.contains(t.substring(dot + 1).toLowerCase());
}

/// One produced file: `GET /v1/conversations/{id}/library`.
class LibraryEntry {
  const LibraryEntry({
    required this.actionId,
    required this.tool,
    required this.hash,
    this.filename,
    this.title,
    this.mime,
    this.createdAt = '',
  });

  factory LibraryEntry.fromJson(Map<String, dynamic> json) {
    return LibraryEntry(
      actionId: json['action_id'] as String,
      tool: json['tool'] as String,
      hash: json['hash'] as String,
      filename: _str(json['filename']),
      title: _str(json['title']),
      mime: _str(json['mime']),
      createdAt: _str(json['created_at']) ?? '',
    );
  }

  final String actionId;
  final String tool;
  final String hash;
  final String? filename;
  final String? title;
  final String? mime;
  final String createdAt;

  String get label => filename ?? title ?? tool;
}

bool validHash(String h) => RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(h);

/// The file formats the server can render a preview for (0.28.0).
const previewableExtensions = ['pdf', 'pptx', 'xlsx', 'docx', 'md'];

/// The preview kind for a file name, or null when the file is not previewable.
PreviewKind? previewKindForName(String name) {
  final lower = name.trim().toLowerCase();
  final dot = lower.lastIndexOf('.');
  if (dot < 0 || dot == lower.length - 1) {
    return null;
  }
  return switch (lower.substring(dot + 1)) {
    'pdf' => PreviewKind.pdf,
    'pptx' => PreviewKind.slide,
    'xlsx' => PreviewKind.sheet,
    'docx' => PreviewKind.doc,
    'md' => PreviewKind.markdown,
    _ => null,
  };
}

enum PreviewKind {
  pdf,
  slide,
  sheet,
  doc,
  markdown;

  String get label => switch (this) {
        PreviewKind.pdf => 'PDF page 1',
        PreviewKind.slide => 'Slide 1',
        PreviewKind.sheet => 'First rows',
        PreviewKind.doc => 'First blocks',
        PreviewKind.markdown => 'Markdown',
      };
}

/// One server-rendered artifact preview (0.28.0):
/// `GET /v1/artifacts/{hash}/preview` -> `{content_type, text?, image_b64?}`.
/// Values are bounded and control characters stripped before display.
class ArtifactPreview {
  const ArtifactPreview({
    required this.contentType,
    this.text,
    this.imageB64,
  });

  static const maxTextChars = 200000;
  static const maxImageB64Chars = 8 * 1024 * 1024;

  static ArtifactPreview? fromJson(Map<String, dynamic> json) {
    final contentType = _str(json['content_type']) ?? '';
    if (contentType.isEmpty || contentType.runes.length > 128) {
      return null;
    }
    String? text = _str(json['text']);
    if (text != null) {
      if (text.runes.length > maxTextChars) {
        text = String.fromCharCodes(text.runes.take(maxTextChars));
      }
      text = text.replaceAll(RegExp(r'[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]'), '');
    }
    String? imageB64 = _str(json['image_b64']);
    if (imageB64 != null) {
      final trimmed = imageB64.trim();
      final valid = trimmed.isNotEmpty &&
          trimmed.runes.length <= maxImageB64Chars &&
          RegExp(r'^[A-Za-z0-9+/]+=*$').hasMatch(trimmed);
      imageB64 = valid ? trimmed : null;
    }
    return ArtifactPreview(
      contentType: contentType,
      text: (text != null && text.trim().isEmpty) ? null : text,
      imageB64: imageB64,
    );
  }

  final String contentType;
  final String? text;
  final String? imageB64;

  /// The image as a data URL, when the server sent a valid one.
  String? get imageDataUrl {
    final b64 = imageB64;
    if (b64 == null) {
      return null;
    }
    final mime = contentType.startsWith('image/') ? contentType : 'image/png';
    return 'data:$mime;base64,$b64';
  }

  bool get hasContent => imageB64 != null || (text != null && text!.isNotEmpty);
}

/// `GET /v1/bots` row.
class BotSummary {
  const BotSummary({
    required this.id,
    this.role = '',
    this.enabled = true,
    this.live = false,
    this.model,
    this.displayName,
    this.description,
    this.avatar,
    this.avatarVersion,
    this.tools = const [],
    this.skills = const [],
    this.requireApproval = const [],
  });

  factory BotSummary.fromJson(Map<String, dynamic> json) {
    List<String> strs(Object? v) =>
        v is List ? v.whereType<String>().toList() : <String>[];
    return BotSummary(
      id: json['id'] as String,
      role: _str(json['role']) ?? '',
      enabled: json['enabled'] as bool? ?? true,
      live: json['live'] as bool? ?? false,
      model: _str(json['model']),
      displayName: _str(json['display_name']),
      description: _str(json['description']),
      avatar: _str(json['avatar']),
      avatarVersion: _str(json['avatar_version']),
      tools: strs(json['tools']),
      skills: strs(json['skills']),
      requireApproval: strs(json['require_approval']),
    );
  }

  final String id;
  final String role;
  final bool enabled;
  final bool live;
  final String? model;
  final String? displayName;
  final String? description;
  final String? avatar;
  final String? avatarVersion;
  final List<String> tools;
  final List<String> skills;
  final List<String> requireApproval;

  String get name {
    final n = displayName;
    return (n != null && n.trim().isNotEmpty) ? n : id;
  }
}

/// `GET /v1/bots/{id}`.
class BotDetail {
  const BotDetail({
    required this.id,
    required this.displayName,
    this.role = '',
    this.description = '',
    this.avatar = '',
    this.enabled = true,
    this.provider = '',
    this.model = '',
    this.instructions,
    this.tools = const [],
    this.skills = const [],
    this.requireApproval = const [],
    this.memoryCount = 0,
    this.artifactCount = 0,
  });

  factory BotDetail.fromJson(Map<String, dynamic> json) {
    List<String> strs(Object? v) =>
        v is List ? v.whereType<String>().toList() : <String>[];
    return BotDetail(
      id: json['id'] as String,
      displayName: _str(json['display_name']) ?? (json['id'] as String),
      role: _str(json['role']) ?? '',
      description: _str(json['description']) ?? '',
      avatar: _str(json['avatar']) ?? '',
      enabled: json['enabled'] as bool? ?? true,
      provider: _str(json['provider']) ?? '',
      model: _str(json['model']) ?? '',
      instructions: _str(json['instructions']),
      tools: strs(json['tools']),
      skills: strs(json['skills']),
      requireApproval: strs(json['require_approval']),
      memoryCount: _int(json['memory_count']),
      artifactCount: _int(json['artifact_count']),
    );
  }

  final String id;
  final String displayName;
  final String role;
  final String description;
  final String avatar;
  final bool enabled;
  final String provider;
  final String model;
  final String? instructions;
  final List<String> tools;
  final List<String> skills;
  final List<String> requireApproval;
  final int memoryCount;
  final int artifactCount;
}

/// Body of `PUT /v1/bots/{id}`; absent fields are not sent. The instructions
/// travel as the server's `prompt` field. [model] `''` clears to default.
Map<String, dynamic> botSettingsBody({
  String? displayName,
  String? description,
  String? instructions,
  bool? enabled,
  ({String provider, String model})? model,
  bool clearModel = false,
  List<String>? tools,
}) {
  return {
    'display_name': ?displayName?.trim(),
    'description': ?description,
    'prompt': ?instructions,
    'enabled': ?enabled,
    if (clearModel)
      'model': null
    else if (model != null)
      'model': {'provider': model.provider, 'model': model.model},
    'tools': ?tools,
  };
}

/// Human labels for the server's tool ids ("abilities").
String abilityLabel(String tool) {
  final words = tool.replaceAll(RegExp(r'[_.-]+'), ' ').trim();
  return words.isEmpty ? tool : words[0].toUpperCase() + words.substring(1);
}

class UsageTotals {
  const UsageTotals({
    this.calls = 0,
    this.promptTokens = 0,
    this.completionTokens = 0,
    this.totalTokens = 0,
  });
  factory UsageTotals.fromJson(Object? v) {
    final j = v is Map<String, dynamic> ? v : const <String, dynamic>{};
    return UsageTotals(
      calls: _int(j['calls']),
      promptTokens: _int(j['prompt_tokens']),
      completionTokens: _int(j['completion_tokens']),
      totalTokens: _int(j['total_tokens']),
    );
  }
  final int calls;
  final int promptTokens;
  final int completionTokens;
  final int totalTokens;
}

/// `quota.daily_tasks` on `GET /v1/usage`: the per-user task budget for the
/// current UTC day. A limit of 0 means no quota is enforced.
class UsageQuota {
  const UsageQuota({
    this.used = 0,
    this.limit = 0,
    this.nearLimit = false,
    this.atLimit = false,
  });
  factory UsageQuota.fromJson(Object? v) {
    final j = v is Map<String, dynamic> ? v : const <String, dynamic>{};
    return UsageQuota(
      used: _int(j['used']),
      limit: _int(j['limit']),
      nearLimit: j['near_limit'] as bool? ?? false,
      atLimit: j['at_limit'] as bool? ?? false,
    );
  }
  final int used;
  final int limit;
  final bool nearLimit;
  final bool atLimit;

  bool get enforced => limit > 0;
}

class UsageReport {
  const UsageReport({
    this.totals = const UsageTotals(),
    this.perModel = const [],
    this.quota = const UsageQuota(),
  });
  factory UsageReport.fromJson(Map<String, dynamic> json) {
    final models = json['per_model'];
    return UsageReport(
      totals: UsageTotals.fromJson(json['totals']),
      quota: UsageQuota.fromJson(
        json['quota'] is Map<String, dynamic>
            ? (json['quota'] as Map<String, dynamic>)['daily_tasks']
            : null,
      ),
      perModel: models is List
          ? models.whereType<Map<String, dynamic>>().map((m) {
              final t = UsageTotals.fromJson(m);
              return (
                provider: _str(m['provider']) ?? '',
                model: _str(m['model']) ?? '',
                totals: t,
              );
            }).toList()
          : const [],
    );
  }
  final UsageTotals totals;
  final List<({String provider, String model, UsageTotals totals})> perModel;
  final UsageQuota quota;
}

/// `GET /v1/connectors` row (read-only on the phone).
class ConnectorStatus {
  const ConnectorStatus({
    required this.id,
    required this.kind,
    this.enabled = false,
    this.connected,
    this.personalState,
    this.account,
  });

  factory ConnectorStatus.fromJson(Map<String, dynamic> json) {
    final p = json['personal'];
    return ConnectorStatus(
      id: json['id'] as String,
      kind: _str(json['kind']) ?? '',
      enabled: json['enabled'] as bool? ?? false,
      connected: json['connected'] as bool?,
      personalState: p is Map<String, dynamic> ? _str(p['state']) : null,
      account: p is Map<String, dynamic> ? _str(p['account']) : null,
    );
  }

  final String id;
  final String kind;
  final bool enabled;
  final bool? connected;
  final String? personalState;
  final String? account;

  bool get isConnected => personalState == 'connected' || connected == true;

  /// Short status for the list: connected / ready / needs setup / off.
  String get statusLabel {
    if (isConnected) {
      return 'Connected';
    }
    if (personalState == 'ready') {
      return 'Ready to connect';
    }
    if (personalState == 'not_set_up') {
      return 'Not set up';
    }
    return enabled ? 'On' : 'Off';
  }

  /// True when linking needs a sign-in page that returns to a PC loopback.
  bool get needsDesktop => !isConnected && personalState == 'ready';
}

class Transcription {
  const Transcription(this.text);
  final String text;
  factory Transcription.fromJson(Map<String, dynamic> json) =>
      Transcription(_str(json['text']) ?? '');
}

// ---------------------------------------------------------------------------
// Event stream

/// One complete SSE frame.
class SseFrame {
  const SseFrame(this.id, this.event, this.data);
  final int? id;
  final String? event;
  final String data;
}

/// Incremental Server-Sent Events parser with a bounded buffer (256 KiB),
/// the same rules as the desktop `SseParser`.
class SseParser {
  static const maxBuffer = 256 * 1024;
  String _buffer = '';
  int? _lastId;
  String? _event;
  final StringBuffer _data = StringBuffer();
  bool _dataSeen = false;

  List<SseFrame> feed(String chunk) {
    final frames = <SseFrame>[];
    if (_buffer.length + chunk.length > maxBuffer) {
      _buffer = '';
      _event = null;
      _data.clear();
      _dataSeen = false;
      return frames;
    }
    _buffer += chunk;
    while (true) {
      final end = _buffer.indexOf('\n');
      if (end < 0) {
        break;
      }
      var line = _buffer.substring(0, end);
      _buffer = _buffer.substring(end + 1);
      if (line.endsWith('\r')) {
        line = line.substring(0, line.length - 1);
      }
      _line(line, frames);
    }
    return frames;
  }

  void _line(String line, List<SseFrame> frames) {
    if (line.isEmpty) {
      if (_data.isEmpty) {
        _event = null;
      } else {
        frames.add(SseFrame(_lastId, _event, _data.toString()));
        _event = null;
        _data.clear();
      }
      _dataSeen = false;
      return;
    }
    if (line.startsWith(':')) {
      return;
    }
    final i = line.indexOf(':');
    if (i < 0) {
      return;
    }
    final field = line.substring(0, i);
    var value = line.substring(i + 1);
    if (value.startsWith(' ')) {
      value = value.substring(1);
    }
    switch (field) {
      case 'id':
        final id = int.tryParse(value);
        if (id != null) {
          _lastId = id;
        }
      case 'event':
        _event = value;
      case 'data':
        if (_dataSeen) {
          _data.write('\n');
        }
        _data.write(value);
        _dataSeen = true;
    }
  }
}

/// A server event: `id` is also the resume cursor.
class ServerEvent {
  const ServerEvent(this.id, this.kind, this.payload);
  final int id;
  final String kind;
  final Map<String, dynamic> payload;

  String? get conversationId => _str(payload['conversation_id']);
  String? get taskId => _str(payload['task_id']);

  static ServerEvent? fromFrame(SseFrame f) {
    final id = f.id;
    if (id == null) {
      return null;
    }
    Object? decoded;
    try {
      decoded = jsonDecode(f.data);
    } catch (_) {
      decoded = null;
    }
    return ServerEvent(
      id,
      f.event ?? '',
      decoded is Map<String, dynamic> ? decoded : const {},
    );
  }
}

enum StepPhase { running, progress, done, failed, waitingApproval }

StepPhase? parseStepPhase(String? v) => switch (v) {
      'running' => StepPhase.running,
      'progress' => StepPhase.progress,
      'done' => StepPhase.done,
      'failed' => StepPhase.failed,
      'waiting_approval' => StepPhase.waitingApproval,
      _ => null,
    };

bool stepSpins(StepPhase phase) =>
    phase == StepPhase.running || phase == StepPhase.progress;

enum StepAction { none, retry, keys, reconnect }

StepAction parseStepAction(String? v) => switch (v) {
      'retry' => StepAction.retry,
      'keys' => StepAction.keys,
      'reconnect' => StepAction.reconnect,
      _ => StepAction.none,
    };

enum ResultKind { audio, image, file, table, link, text }

ResultKind parseResultKind(String? v) => switch (v) {
      'audio' => ResultKind.audio,
      'image' => ResultKind.image,
      'file' => ResultKind.file,
      'table' => ResultKind.table,
      'link' => ResultKind.link,
      _ => ResultKind.text,
    };

class StepResult {
  StepResult({
    required this.kind,
    this.title,
    this.url,
    this.mime,
    this.voice,
    this.model,
    this.name,
    this.size,
    this.text,
    this.artifact,
    this.columns = const [],
    this.rows = const [],
  });

  final ResultKind kind;
  final String? title;
  final String? url;
  final String? mime;
  final String? voice;
  final String? model;
  final String? name;
  final int? size;
  final String? text;
  final String? artifact;
  final List<String> columns;
  final List<List<String>> rows;
}

class ActivityStep {
  ActivityStep({
    required this.callId,
    required this.label,
    required this.phase,
    this.approvalId,
    this.tool,
    this.title,
    this.inputSummary,
    this.resultSummary,
    this.error,
    this.action = StepAction.none,
    this.durationMs,
    this.startedMs,
    this.result,
  });

  final String callId;
  String label;
  StepPhase phase;
  String? approvalId;
  String? tool;
  String? title;
  String? inputSummary;
  String? resultSummary;
  String? error;
  StepAction action;
  int? durationMs;
  int? startedMs;
  StepResult? result;

  String get heading => title ?? label;
}

/// "under 1s", "12s", "1m 4s".
String formatDuration(int ms) {
  if (ms < 1000) {
    return 'under 1s';
  }
  if (ms < 60000) {
    return '${(ms + 999) ~/ 1000}s';
  }
  final minutes = ms ~/ 60000;
  final seconds = (ms % 60000) ~/ 1000;
  if (seconds == 0) {
    return '${minutes}m';
  }
  return '${minutes}m ${seconds}s';
}

/// "5 steps" or "5 steps, 12s".
String formatStepsSummary(int count, int? durationMs) {
  final steps = count == 1 ? '1 step' : '$count steps';
  if (durationMs == null) {
    return steps;
  }
  return '$steps, ${formatDuration(durationMs)}';
}

const _mediaHosts = ['elevenlabs.io', 'storage.googleapis.com'];

bool safeMediaUrl(String raw) {
  final uri = Uri.tryParse(raw.trim());
  if (uri == null || uri.scheme != 'https' || uri.userInfo.isNotEmpty) {
    return false;
  }
  if (uri.query.isNotEmpty) {
    return false;
  }
  final host = uri.host.toLowerCase();
  return _mediaHosts.any((name) => host == name || host.endsWith('.$name'));
}

bool safeLinkUrl(String raw) {
  final uri = Uri.tryParse(raw.trim());
  if (uri == null || uri.scheme != 'https' || uri.userInfo.isNotEmpty) {
    return false;
  }
  return uri.host.isNotEmpty && uri.query.isEmpty;
}

/// A provider body or a JSON blob becomes a sentence.
String presentError(String text) {
  final trimmed = text.trimLeft();
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    return 'This step failed.';
  }
  return text;
}

String? _clean(Object? v, [int max = 120]) {
  if (v is! String) {
    return null;
  }
  final s = String.fromCharCodes(
    v.runes.where((r) => !(r <= 0x1f || (r >= 0x7f && r <= 0x9f))),
  );
  final cut = s.runes.length > max
      ? String.fromCharCodes(s.runes.take(max))
      : s;
  return cut.trim().isEmpty ? null : cut;
}

/// Live steps of the newest turn, built from `turn.step` events. Untrusted
/// display text: control characters dropped, length bounded, at most 64 steps.
class Activity {
  String taskId = '';
  final List<ActivityStep> steps = [];

  bool get isEmpty => steps.isEmpty;

  void clear() {
    taskId = '';
    steps.clear();
  }

  bool apply(String kind, Map<String, dynamic> p) {
    if (kind != 'turn.step') {
      return false;
    }
    final task = _clean(p['task_id']);
    final call = _clean(p['call_id']) ?? _clean(p['label']);
    final label = _clean(p['label']);
    final phase = parseStepPhase(p['phase'] as String?) ??
        (label != null ? StepPhase.running : null);
    if (task == null || call == null || label == null || phase == null) {
      return false;
    }
    if (taskId != task) {
      steps.clear();
      taskId = task;
    }
    final approval = _clean(p['approval_id']);
    final tool = _clean(p['tool']);
    final title = toolHeading(tool);
    final inputSummary = _clean(p['input_summary'], 160);
    final resultSummary = _clean(p['result_summary']);
    final rawError = _clean(p['error'], 200);
    final error = rawError == null ? null : presentError(rawError);
    final duration = _duration(p['duration_ms']);
    final result = _firstCard(p['cards']);
    final existing = steps.where((s) => s.callId == call);
    if (existing.isNotEmpty) {
      final s = existing.first;
      s.phase = phase;
      s.label = label;
      s.tool = tool ?? s.tool;
      s.title = title ?? s.title;
      s.inputSummary = inputSummary ?? s.inputSummary;
      s.resultSummary = resultSummary ?? s.resultSummary;
      s.result = result ?? s.result;
      if (duration != null) {
        s.durationMs = duration;
      }
      if (phase == StepPhase.waitingApproval) {
        s.approvalId = approval ?? s.approvalId;
        s.error = null;
        s.action = StepAction.none;
      } else if (phase == StepPhase.failed) {
        s.approvalId = null;
        s.error = error ?? s.error;
        s.action = actionForError(s.error);
      } else {
        s.approvalId = null;
        s.error = null;
        s.action = StepAction.none;
      }
    } else if (steps.length < 64) {
      steps.add(ActivityStep(
        callId: call,
        label: label,
        phase: phase,
        approvalId: phase == StepPhase.waitingApproval ? approval : null,
        tool: tool,
        title: title,
        inputSummary: inputSummary,
        resultSummary: resultSummary,
        error: phase == StepPhase.failed ? error : null,
        action: phase == StepPhase.failed ? actionForError(error) : StepAction.none,
        durationMs: duration,
        result: result,
      ));
    }
    return true;
  }

  int? get totalDurationMs {
    final finished = steps.where((s) => !stepSpins(s.phase)).toList();
    if (finished.isEmpty || finished.any((s) => s.durationMs == null)) {
      return null;
    }
    return finished.fold<int>(0, (sum, s) => sum + (s.durationMs ?? 0));
  }

  String get summary => formatStepsSummary(steps.length, totalDurationMs);
}

String? _sha(Object? v) {
  final text = _clean(v, 64);
  if (text == null || !RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(text)) {
    return null;
  }
  return text.toLowerCase();
}

String? _shaFromPath(String? url) {
  if (url == null || !url.startsWith('/v1/artifacts/')) {
    return null;
  }
  return _sha(url.substring('/v1/artifacts/'.length).split('?').first);
}

int? _duration(Object? v) {
  if (v is! num) {
    return null;
  }
  final ms = v.toInt();
  if (ms < 0 || ms > 3600000) {
    return null;
  }
  return ms;
}

String? toolHeading(String? tool) {
  if (tool == null || tool.isEmpty) {
    return null;
  }
  if (tool.startsWith('elevenlabs_') || tool.startsWith('audio_')) {
    return 'ElevenLabs';
  }
  if (tool.startsWith('browser_')) {
    return 'Browser';
  }
  if (tool.startsWith('slack_')) {
    return 'Slack';
  }
  if (tool.startsWith('github_')) {
    return 'GitHub';
  }
  if (tool == 'media_job') {
    return 'Media';
  }
  final word = tool.split(RegExp(r'[_.-]')).firstWhere((part) => part.isNotEmpty, orElse: () => 'Tool');
  if (word.isEmpty) {
    return 'Tool';
  }
  return word[0].toUpperCase() + word.substring(1);
}

StepAction actionForError(String? error) {
  final lower = (error ?? '').toLowerCase();
  if (lower.contains('api key') || lower.contains('api_key') || lower.contains('credential') || lower.contains('provider key')) {
    return StepAction.keys;
  }
  if (lower.contains('timed out') || lower.contains('timeout') || lower.contains('you can retry') || lower.contains('busy')) {
    return StepAction.retry;
  }
  if (lower.contains('could not be reached') || lower.contains('check the connection') || lower.contains('reconnect')) {
    return StepAction.reconnect;
  }
  return StepAction.none;
}

StepResult? _firstCard(Object? raw) {
  if (raw is! List || raw.isEmpty) {
    return null;
  }
  final first = raw.first;
  if (first is! Map<String, dynamic>) {
    return null;
  }
  return _parseResult(first);
}

StepResult? _parseResult(Map<String, dynamic> raw) {
  final kindName = raw['type'] as String? ?? raw['kind'] as String?;
  if (kindName != 'audio' && kindName != 'image' && kindName != 'file' && kindName != 'table' && kindName != 'link') {
    return null;
  }
  final kind = parseResultKind(kindName);
  final url = _clean(raw['url'], 300);
  final safeUrl = url != null && (safeLinkUrl(url) || safeMediaUrl(url)) ? url : null;
  final hash = _sha(raw['artifact_hash']) ?? _sha(raw['artifact']) ?? _shaFromPath(url);
  final columns = <String>[];
  final cols = raw['columns'];
  if (cols is List) {
    for (final cell in cols.take(8)) {
      final text = _clean(cell, 80);
      if (text != null) {
        columns.add(text);
      }
    }
  }
  final rows = <List<String>>[];
  final body = raw['rows'];
  if (body is List && columns.isNotEmpty) {
    for (final row in body.take(12)) {
      if (row is! List) {
        continue;
      }
      rows.add([
        for (var i = 0; i < columns.length; i++)
          i < row.length ? (_clean(row[i], 80) ?? '') : '',
      ]);
    }
  }
  final result = StepResult(
    kind: kind == ResultKind.table && columns.isEmpty ? ResultKind.text : kind,
    title: _clean(raw['title']),
    url: safeUrl,
    mime: _clean(raw['mime'], 80),
    voice: _clean(raw['voice'], 40),
    model: _clean(raw['model'], 40),
    name: _clean(raw['filename'], 80) ?? _clean(raw['name'], 80),
    size: raw['bytes'] is num
        ? (raw['bytes'] as num).toInt()
        : (raw['size'] is num ? (raw['size'] as num).toInt() : null),
    text: _clean(raw['description'], 400) ?? _clean(raw['alt']) ?? _clean(raw['text'], 400),
    artifact: hash,
    columns: columns,
    rows: rows,
  );
  final useful = result.title != null ||
      result.url != null ||
      result.text != null ||
      result.artifact != null ||
      result.name != null ||
      result.columns.isNotEmpty;
  return useful ? result : null;
}

enum ChatAction { retry, reconnect, keys, none }

class ChatNotice {
  const ChatNotice(this.message, this.action);
  final String message;
  final ChatAction action;
}

/// A sentence and an action for a failed send. JSON is never shown.
ChatNotice classifySendError({
  required String kind,
  int? status,
  String? detail,
  String? code,
}) {
  if (kind == 'transport') {
    return const ChatNotice(
      "Can't reach SeraBot. Your message is still in the box. Check the connection, then send it again.",
      ChatAction.retry,
    );
  }
  if (status == 408 || status == 504) {
    return const ChatNotice(
      'The request timed out. Your message is still in the box. Send it again.',
      ChatAction.retry,
    );
  }
  if (status != null && status >= 500 && status <= 599) {
    return const ChatNotice(
      'SeraBot had a problem sending that message. Your text is still in the box. Try again.',
      ChatAction.retry,
    );
  }
  if (status == 429) {
    return const ChatNotice(
      'Too many requests right now. Your message is still in the box. Wait a moment, then send it again.',
      ChatAction.retry,
    );
  }
  final sentence = detail ?? '';
  if (sentence.trimLeft().startsWith('{') || sentence.trimLeft().startsWith('[')) {
    return const ChatNotice(
      'Something went wrong sending that message. Your text is still in the box. Try again.',
      ChatAction.retry,
    );
  }
  if (_keyish(sentence) || _keyish(code ?? '')) {
    return const ChatNotice(
      'The provider rejected the API key. Open AI keys and check it. Your message is still in the box.',
      ChatAction.keys,
    );
  }
  if (sentence.isEmpty) {
    return const ChatNotice(
      'Something went wrong sending that message. Your text is still in the box. Try again.',
      ChatAction.retry,
    );
  }
  return ChatNotice(sentence, ChatAction.none);
}

bool _keyish(String text) {
  final lower = text.toLowerCase();
  return lower.contains('api key') ||
      lower.contains('api_key') ||
      lower.contains('provider key') ||
      lower.contains('invalid key') ||
      lower.contains('missing key');
}

class ChatPlanItem {
  ChatPlanItem(this.text, this.status);
  final String text;
  final String status;
}

class ChatPlan {
  ChatPlan(this.goal, this.items);
  final String goal;
  final List<ChatPlanItem> items;

  int get done => items.where((item) => item.status == 'done').length;

  String get progress => '$done of ${items.length} done';

  static ChatPlan? parse(Map<String, dynamic>? raw) {
    if (raw == null) {
      return null;
    }
    final goal = _clean(raw['goal'], 200);
    final list = raw['items'];
    if (goal == null || list is! List || list.length > 20) {
      return null;
    }
    final items = <ChatPlanItem>[];
    for (final item in list) {
      if (item is! Map<String, dynamic>) {
        return null;
      }
      final text = _clean(item['text'], 160);
      final status = item['status'];
      if (text == null || status is! String || !const {'pending', 'doing', 'done'}.contains(status)) {
        return null;
      }
      items.add(ChatPlanItem(text, status));
    }
    return ChatPlan(goal, items);
  }
}
