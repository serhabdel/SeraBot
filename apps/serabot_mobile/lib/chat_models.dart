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

class UsageReport {
  const UsageReport({
    this.totals = const UsageTotals(),
    this.perModel = const [],
  });
  factory UsageReport.fromJson(Map<String, dynamic> json) {
    final models = json['per_model'];
    return UsageReport(
      totals: UsageTotals.fromJson(json['totals']),
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

enum StepPhase { running, done, failed, waitingApproval }

StepPhase? parseStepPhase(String? v) => switch (v) {
      'running' => StepPhase.running,
      'done' => StepPhase.done,
      'failed' => StepPhase.failed,
      'waiting_approval' => StepPhase.waitingApproval,
      _ => null,
    };

class ActivityStep {
  ActivityStep(this.callId, this.label, this.phase, this.approvalId);
  final String callId;
  String label;
  StepPhase phase;
  String? approvalId;
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
    final existing = steps.where((s) => s.callId == call);
    if (existing.isNotEmpty) {
      final s = existing.first;
      s.phase = phase;
      s.label = label;
      s.approvalId =
          phase == StepPhase.waitingApproval ? (approval ?? s.approvalId) : null;
    } else if (steps.length < 64) {
      steps.add(ActivityStep(call, label, phase, approval));
    }
    return true;
  }

  String get summary => steps.length == 1 ? '1 step' : '${steps.length} steps';
}
