import 'chat_models.dart';

class Selection {
  const Selection({required this.provider, required this.model});
  final String provider;
  final String model;

  bool get isAuto => provider == 'auto';

  factory Selection.fromJson(Map<String, dynamic> json) {
    return Selection(
      provider: json['provider'] as String? ?? '',
      model: json['model'] as String? ?? '',
    );
  }

  Map<String, dynamic> toJson() => {'provider': provider, 'model': model};

  static Selection auto() => const Selection(provider: 'auto', model: '');
}

class ProviderInfo {
  ProviderInfo({
    required this.id,
    required this.models,
    required this.dataEgressApproved,
  });

  factory ProviderInfo.fromJson(Map<String, dynamic> json) {
    return ProviderInfo(
      id: json['id'] as String,
      models: (json['models'] as List<dynamic>).cast<String>(),
      dataEgressApproved: json['data_egress_approved'] as bool? ?? false,
    );
  }

  final String id;
  final List<String> models;
  final bool dataEgressApproved;
}

class CatalogEntry {
  CatalogEntry({
    required this.provider,
    required this.model,
    required this.dataEgressApproved,
  });

  factory CatalogEntry.fromJson(Map<String, dynamic> json) {
    return CatalogEntry(
      provider: json['provider'] as String,
      model: json['model'] as String,
      dataEgressApproved: json['data_egress_approved'] as bool? ?? false,
    );
  }

  final String provider;
  final String model;
  final bool dataEgressApproved;
}

class Catalog {
  Catalog({
    required this.providers,
    required this.entries,
    required this.selection,
    this.effectiveSelection,
  });

  factory Catalog.fromJson(Map<String, dynamic> json) {
    return Catalog(
      providers: (json['providers'] as List<dynamic>)
          .map((e) => ProviderInfo.fromJson(e as Map<String, dynamic>))
          .toList(),
      entries: (json['entries'] as List<dynamic>? ?? [])
          .map((e) => CatalogEntry.fromJson(e as Map<String, dynamic>))
          .toList(),
      selection: Selection.fromJson(json['selection'] as Map<String, dynamic>),
      effectiveSelection: json['effective_selection'] != null
          ? Selection.fromJson(
              json['effective_selection'] as Map<String, dynamic>,
            )
          : null,
    );
  }

  final List<ProviderInfo> providers;
  final List<CatalogEntry> entries;
  final Selection selection;
  final Selection? effectiveSelection;

  bool allows(Selection candidate) {
    if (candidate.isAuto) {
      final effective = effectiveSelection;
      if (effective == null) {
        return false;
      }
      return allowsConcrete(effective);
    }
    return allowsConcrete(candidate);
  }

  bool allowsConcrete(Selection candidate) {
    for (final p in providers) {
      if (p.id == candidate.provider &&
          p.dataEgressApproved &&
          p.models.contains(candidate.model)) {
        return true;
      }
    }
    return false;
  }
}

enum MessageRole { user, assistant }

MessageRole parseRole(String raw) {
  switch (raw) {
    case 'user':
      return MessageRole.user;
    case 'assistant':
      return MessageRole.assistant;
    default:
      throw FormatException('unknown role');
  }
}

class ChatMessage {
  ChatMessage({
    required this.id,
    required this.role,
    required this.content,
    required this.createdAt,
    this.name,
    this.bot,
    this.attachments = const [],
  });

  factory ChatMessage.fromJson(Map<String, dynamic> json) {
    final att = json['attachments'];
    return ChatMessage(
      id: json['id'] as String,
      role: parseRole(json['role'] as String),
      content: json['content'] as String,
      createdAt: json['created_at'] as String,
      name: json['name'] as String?,
      bot: json['bot'] as String?,
      attachments: att is List
          ? att
              .whereType<Map<String, dynamic>>()
              .map(AttachmentInfo.fromJson)
              .toList()
          : const [],
    );
  }

  final String id;
  final MessageRole role;
  final String content;
  final String createdAt;

  /// Bot id on a room or handoff reply; null on ordinary assistant text.
  final String? name;

  /// Bot that produced this assistant message.
  final String? bot;
  final List<AttachmentInfo> attachments;

  /// Who spoke, for the bubble header: the room speaker, else the bot.
  String? get speaker => name ?? bot;
}

enum TaskStatus {
  queued,
  running,
  awaitingApproval,
  needsReview,
  done,
  failed,
  cancelled,
}

TaskStatus parseTaskStatus(String raw) {
  switch (raw) {
    case 'queued':
      return TaskStatus.queued;
    case 'running':
      return TaskStatus.running;
    case 'awaiting_approval':
      return TaskStatus.awaitingApproval;
    case 'needs_review':
      return TaskStatus.needsReview;
    case 'done':
      return TaskStatus.done;
    case 'failed':
      return TaskStatus.failed;
    case 'cancelled':
      return TaskStatus.cancelled;
    default:
      throw FormatException('unknown status');
  }
}

extension TaskStatusX on TaskStatus {
  bool get terminal =>
      this == TaskStatus.done ||
      this == TaskStatus.failed ||
      this == TaskStatus.cancelled;

  String get label {
    switch (this) {
      case TaskStatus.queued:
        return 'Queued';
      case TaskStatus.running:
        return 'Working';
      case TaskStatus.awaitingApproval:
        return 'Awaiting approval';
      case TaskStatus.needsReview:
        return 'Needs review';
      case TaskStatus.done:
        return 'Done';
      case TaskStatus.failed:
        return 'Failed';
      case TaskStatus.cancelled:
        return 'Cancelled';
    }
  }
}

class TaskInfo {
  TaskInfo({
    required this.id,
    required this.kind,
    required this.bot,
    required this.status,
  });

  factory TaskInfo.fromJson(Map<String, dynamic> json) {
    return TaskInfo(
      id: json['id'] as String,
      kind: json['kind'] as String? ?? '',
      bot: json['bot'] as String? ?? '',
      status: parseTaskStatus(json['status'] as String),
    );
  }

  final String id;
  final String kind;
  final String bot;
  final TaskStatus status;
}

class ApprovalItem {
  ApprovalItem({
    required this.id,
    required this.actionId,
    required this.tool,
    required this.summary,
    required this.expiresAt,
    this.conversationId,
    this.destination,
  });

  factory ApprovalItem.fromJson(Map<String, dynamic> json) {
    final conv = json['conversation_id'];
    final dest = json['destination'];
    return ApprovalItem(
      id: json['id'] as String,
      actionId: json['action_id'] as String,
      tool: json['tool'] as String,
      summary: json['summary'] as String,
      expiresAt: json['expires_at'] as String,
      conversationId: conv is String && conv.trim().isNotEmpty ? conv : null,
      destination: dest is String && dest.trim().isNotEmpty ? dest : null,
    );
  }

  final String id;
  final String actionId;
  final String tool;
  final String summary;
  final String expiresAt;
  final String? conversationId;
  final String? destination;
}

class HealthInfo {
  HealthInfo({required this.ok, required this.version});

  factory HealthInfo.fromJson(Map<String, dynamic> json) {
    return HealthInfo(
      ok: json['ok'] as bool? ?? false,
      version: json['version'] as String? ?? '',
    );
  }

  final bool ok;
  final String version;
}

/// One model-backed capability (speech-to-text or OCR) and whether it can run.
class VoiceModelStatus {
  VoiceModelStatus({required this.model, required this.ready, this.detail});

  factory VoiceModelStatus.fromJson(Map<String, dynamic> json) {
    return VoiceModelStatus(
      model: json['model'] as String? ?? '',
      ready: json['ready'] as bool? ?? false,
      detail: json['detail'] as String?,
    );
  }

  final String model;
  final bool ready;

  /// Why it is not ready; names a setting, never a secret.
  final String? detail;
}

/// Text-to-speech readiness plus the configured voice.
class VoiceSpeechStatus extends VoiceModelStatus {
  VoiceSpeechStatus({
    required super.model,
    required super.ready,
    super.detail,
    required this.voiceId,
  });

  factory VoiceSpeechStatus.fromJson(Map<String, dynamic> json) {
    return VoiceSpeechStatus(
      model: json['model'] as String? ?? '',
      ready: json['ready'] as bool? ?? false,
      detail: json['detail'] as String?,
      voiceId: json['voice_id'] as String? ?? '',
    );
  }

  final String voiceId;
}

/// `GET /v1/settings/voice`.
class VoiceSettings {
  VoiceSettings({
    required this.stt,
    required this.tts,
    required this.ocr,
    required this.replyMode,
    required this.maxSpeechChars,
  });

  factory VoiceSettings.fromJson(Map<String, dynamic> json) {
    return VoiceSettings(
      stt: VoiceModelStatus.fromJson(
          (json['stt'] as Map<String, dynamic>?) ?? const {}),
      tts: VoiceSpeechStatus.fromJson(
          (json['tts'] as Map<String, dynamic>?) ?? const {}),
      ocr: VoiceModelStatus.fromJson(
          (json['ocr'] as Map<String, dynamic>?) ?? const {}),
      replyMode: json['reply_mode'] as String? ?? 'text',
      maxSpeechChars: json['max_speech_chars'] as int? ?? 0,
    );
  }

  final VoiceModelStatus stt;
  final VoiceSpeechStatus tts;
  final VoiceModelStatus ocr;
  final String replyMode;
  final int maxSpeechChars;
}

/// `GET/PUT /v1/conversations/{id}/reply-mode`.
class ReplyMode {
  ReplyMode({
    required this.conversationId,
    required this.mode,
    required this.voiceReady,
    this.detail,
  });

  factory ReplyMode.fromJson(Map<String, dynamic> json) {
    return ReplyMode(
      conversationId: json['conversation_id'] as String? ?? '',
      mode: json['mode'] as String? ?? 'text',
      voiceReady: json['voice_ready'] as bool? ?? false,
      detail: json['detail'] as String?,
    );
  }

  final String conversationId;
  final String mode;
  final bool voiceReady;
  final String? detail;
}

enum ApprovalDecision { once, always, deny }

enum ConversationKind { section, group, direct, room }

extension ConversationKindX on ConversationKind {
  String get apiValue {
    switch (this) {
      case ConversationKind.section:
        return 'section';
      case ConversationKind.group:
        return 'group';
      case ConversationKind.direct:
        return 'direct';
      case ConversationKind.room:
        return 'room';
    }
  }

  String get label {
    switch (this) {
      case ConversationKind.section:
        return 'Section';
      case ConversationKind.group:
        return 'Group';
      case ConversationKind.direct:
        return 'Direct';
      case ConversationKind.room:
        return 'Room';
    }
  }

  String get idPrefix {
    switch (this) {
      case ConversationKind.section:
        return 'sec';
      case ConversationKind.group:
        return 'grp';
      case ConversationKind.direct:
        return 'dir';
      case ConversationKind.room:
        return 'room';
    }
  }

  bool get acceptsTurns =>
      this == ConversationKind.direct || this == ConversationKind.room;

  String get parentHint {
    switch (this) {
      case ConversationKind.section:
        return 'No parent. Sections are the top of the tree.';
      case ConversationKind.group:
        return 'Requires an existing section, for example sec:finance.';
      case ConversationKind.direct:
      case ConversationKind.room:
        return 'Optional parent: a section (sec:) or group (grp:).';
    }
  }
}

class CreatedBot {
  CreatedBot({
    required this.id,
    required this.enabled,
    required this.role,
    required this.live,
    required this.tools,
    required this.skills,
    required this.requireApproval,
    required this.botFile,
    required this.promptFile,
  });

  factory CreatedBot.fromJson(Map<String, dynamic> json) {
    return CreatedBot(
      id: json['id'] as String,
      enabled: json['enabled'] as bool,
      role: json['role'] as String,
      live: json['live'] as bool,
      tools: _stringList(json['tools']),
      skills: _stringList(json['skills']),
      requireApproval: _stringList(json['require_approval']),
      botFile: json['bot_file'] as String,
      promptFile: json['prompt_file'] as String,
    );
  }

  final String id;
  final bool enabled;
  final String role;
  final bool live;
  final List<String> tools;
  final List<String> skills;
  final List<String> requireApproval;
  final String botFile;
  final String promptFile;
}

class CreatedConversation {
  CreatedConversation({
    required this.id,
    required this.kind,
    required this.parentId,
    required this.acceptsTurns,
  });

  factory CreatedConversation.fromJson(Map<String, dynamic> json) {
    return CreatedConversation(
      id: json['id'] as String,
      kind: json['kind'] as String,
      parentId: json['parent_id'] as String?,
      acceptsTurns: json['accepts_turns'] as bool,
    );
  }

  final String id;
  final String kind;
  final String? parentId;
  final bool acceptsTurns;
}

class CreatedSkill {
  CreatedSkill({
    required this.id,
    required this.name,
    required this.description,
    required this.file,
    required this.live,
  });

  factory CreatedSkill.fromJson(Map<String, dynamic> json) {
    return CreatedSkill(
      id: json['id'] as String,
      name: json['name'] as String,
      description: json['description'] as String,
      file: json['file'] as String,
      live: json['live'] as bool,
    );
  }

  final String id;
  final String name;
  final String description;
  final String file;
  final bool live;
}

class SkillSummary {
  SkillSummary({
    required this.id,
    required this.name,
    required this.description,
    this.pack,
  });

  factory SkillSummary.fromJson(Map<String, dynamic> json) {
    return SkillSummary(
      id: json['id'] as String,
      name: json['name'] as String,
      description: json['description'] as String,
      pack: json['pack'] as String?,
    );
  }

  final String id;
  final String name;
  final String description;
  final String? pack;
}

List<String> _stringList(dynamic raw) {
  if (raw is! List) {
    throw const FormatException('expected list');
  }
  return raw.map((entry) {
    if (entry is! String) {
      throw const FormatException('expected string');
    }
    return entry;
  }).toList();
}

extension ApprovalDecisionX on ApprovalDecision {
  String get apiValue {
    switch (this) {
      case ApprovalDecision.once:
        return 'once';
      case ApprovalDecision.always:
        return 'always';
      case ApprovalDecision.deny:
        return 'deny';
    }
  }

  String get label {
    switch (this) {
      case ApprovalDecision.once:
        return 'Approve once';
      case ApprovalDecision.always:
        return 'Always allow';
      case ApprovalDecision.deny:
        return 'Deny';
    }
  }
}

/// The signed-in user, as returned by the handoff and GET /v1/me.
class UserInfo {
  const UserInfo({
    required this.id,
    required this.email,
    required this.displayName,
    required this.role,
  });

  factory UserInfo.fromJson(Map<String, dynamic> json) {
    final id = json['id'];
    if (id == null) {
      throw const FormatException('user id missing');
    }
    return UserInfo(
      id: id.toString(),
      email: json['email'] as String? ?? '',
      displayName: json['display_name'] as String? ?? '',
      role: json['role'] as String? ?? '',
    );
  }

  final String id;
  final String email;
  final String displayName;
  final String role;

  /// Name to show in the UI: display name, else email.
  String get label => displayName.isNotEmpty ? displayName : email;
}

/// Personal task row from `GET /v1/tasks`.
class PersonalTask {
  const PersonalTask({
    required this.id,
    required this.title,
    this.notes = '',
    this.status = 'todo',
    this.due,
    this.position = 0,
    this.remoteProvider,
    this.createdAt = '',
    this.updatedAt = '',
  });

  factory PersonalTask.fromJson(Map<String, dynamic> json) {
    return PersonalTask(
      id: json['id']?.toString() ?? '',
      title: json['title'] as String? ?? '',
      notes: json['notes'] as String? ?? '',
      status: json['status'] as String? ?? 'todo',
      due: json['due'] as String?,
      position: (json['position'] as num?)?.toInt() ?? 0,
      remoteProvider: json['remote_provider'] as String?,
      createdAt: json['created_at'] as String? ?? '',
      updatedAt: json['updated_at'] as String? ?? '',
    );
  }

  final String id;
  final String title;
  final String notes;
  final String status;
  final String? due;
  final int position;
  final String? remoteProvider;
  final String createdAt;
  final String updatedAt;

  bool get isDone => status == 'done';
}

class TaskSyncStatus {
  const TaskSyncStatus({
    this.provider = 'none',
    this.health = 'off',
    this.lastRun,
    this.lastOk,
    this.lastError,
  });

  factory TaskSyncStatus.fromJson(Map<String, dynamic>? json) {
    if (json == null) {
      return const TaskSyncStatus();
    }
    return TaskSyncStatus(
      provider: json['provider'] as String? ?? 'none',
      health: json['health'] as String? ?? 'off',
      lastRun: json['last_run'] as String?,
      lastOk: json['last_ok'] as bool?,
      lastError: json['last_error'] as String?,
    );
  }

  final String provider;
  final String health;
  final String? lastRun;
  final bool? lastOk;
  final String? lastError;

  bool get enabled => provider.isNotEmpty && provider != 'none';
}

class TaskCounts {
  const TaskCounts({this.todo = 0, this.inProgress = 0, this.done = 0});

  factory TaskCounts.fromJson(Map<String, dynamic>? json) {
    if (json == null) {
      return const TaskCounts();
    }
    return TaskCounts(
      todo: (json['todo'] as num?)?.toInt() ?? 0,
      inProgress: (json['in_progress'] as num?)?.toInt() ?? 0,
      done: (json['done'] as num?)?.toInt() ?? 0,
    );
  }

  final int todo;
  final int inProgress;
  final int done;

  int get open => todo + inProgress;
}

class TasksResponse {
  const TasksResponse({
    this.tasks = const [],
    this.counts = const TaskCounts(),
    this.sync = const TaskSyncStatus(),
    this.supported = true,
  });

  factory TasksResponse.fromJson(Map<String, dynamic> json) {
    final list = json['tasks'] as List<dynamic>? ?? const [];
    return TasksResponse(
      tasks: list
          .whereType<Map<String, dynamic>>()
          .map(PersonalTask.fromJson)
          .toList(),
      counts: TaskCounts.fromJson(json['counts'] as Map<String, dynamic>?),
      sync: TaskSyncStatus.fromJson(json['sync'] as Map<String, dynamic>?),
      supported: true,
    );
  }

  final List<PersonalTask> tasks;
  final TaskCounts counts;
  final TaskSyncStatus sync;
  final bool supported;
}

class CalendarEventItem {
  const CalendarEventItem({
    required this.id,
    required this.title,
    this.start = '',
    this.end,
    this.allDay = false,
    this.provider,
  });

  factory CalendarEventItem.fromJson(Map<String, dynamic> json) {
    return CalendarEventItem(
      id: json['id']?.toString() ?? '',
      title: json['title'] as String? ?? '(Untitled)',
      start: json['start'] as String? ?? json['due'] as String? ?? '',
      end: json['end'] as String?,
      allDay: json['all_day'] as bool? ?? false,
      provider: json['provider'] as String?,
    );
  }

  final String id;
  final String title;
  final String start;
  final String? end;
  final bool allDay;
  final String? provider;
}

class CalendarProviderWarning {
  const CalendarProviderWarning({
    required this.provider,
    this.statusCode,
    required this.message,
    this.hint,
  });

  factory CalendarProviderWarning.fromJson(Map<String, dynamic> json) {
    final hintRaw = json['hint'] as String?;
    return CalendarProviderWarning(
      provider: json['provider'] as String? ?? '',
      statusCode: (json['status_code'] as num?)?.toInt() ??
          (json['statusCode'] as num?)?.toInt(),
      message: json['message'] as String? ?? '',
      hint: hintRaw != null && hintRaw.trim().isNotEmpty ? hintRaw : null,
    );
  }

  final String provider;
  final int? statusCode;
  final String message;
  final String? hint;
}

class CalendarEventsResult {
  const CalendarEventsResult({
    this.events = const [],
    this.warnings = const [],
  });

  factory CalendarEventsResult.fromJson(Map<String, dynamic> json) {
    final evList = json['events'] as List<dynamic>? ?? const [];
    final warnList = (json['provider_warnings'] ?? json['warnings'])
            as List<dynamic>? ??
        const [];
    return CalendarEventsResult(
      events: evList
          .whereType<Map<String, dynamic>>()
          .map(CalendarEventItem.fromJson)
          .toList(),
      warnings: warnList
          .whereType<Map<String, dynamic>>()
          .map(CalendarProviderWarning.fromJson)
          .toList(),
    );
  }

  final List<CalendarEventItem> events;
  final List<CalendarProviderWarning> warnings;
}

class ProviderQuotaWindow {
  const ProviderQuotaWindow({
    required this.scope,
    required this.label,
    this.usedPercent,
    this.remaining,
    this.resetsAt,
  });

  factory ProviderQuotaWindow.fromJson(Map<String, dynamic> json) {
    return ProviderQuotaWindow(
      scope: json['scope'] as String? ?? '',
      label: json['label'] as String? ?? '',
      usedPercent: (json['used_percent'] as num?)?.toDouble(),
      remaining: (json['remaining'] as num?)?.toDouble(),
      resetsAt: json['resets_at'] as String?,
    );
  }

  final String scope;
  final String label;
  final double? usedPercent;
  final double? remaining;
  final String? resetsAt;

  /// Fraction remaining in `[0.0, 1.0]`.
  double get remainingFraction {
    if (remaining != null) {
      return remaining!.clamp(0.0, 1.0);
    }
    if (usedPercent != null) {
      return ((100.0 - usedPercent!) / 100.0).clamp(0.0, 1.0);
    }
    return 0.0;
  }

  int get remainingPercent => (remainingFraction * 100).round().clamp(0, 100);
}

class ProviderQuotaSnapshot {
  const ProviderQuotaSnapshot({
    this.sampledAt,
    this.source,
    this.state = 'ok',
    this.windows = const [],
  });

  factory ProviderQuotaSnapshot.fromJson(Map<String, dynamic> json) {
    final rawWindows = json['windows'] as List<dynamic>? ?? const [];
    return ProviderQuotaSnapshot(
      sampledAt: json['sampled_at'] as String?,
      source: json['source'] as String?,
      state: json['state'] as String? ?? 'ok',
      windows: rawWindows
          .whereType<Map<String, dynamic>>()
          .map(ProviderQuotaWindow.fromJson)
          .toList(),
    );
  }

  final String? sampledAt;
  final String? source;
  final String state;
  final List<ProviderQuotaWindow> windows;
}

class ProviderAccountSummary {
  const ProviderAccountSummary({
    required this.id,
    required this.label,
    required this.authKind,
    required this.status,
    this.email,
    this.plan,
    this.quota,
  });

  factory ProviderAccountSummary.fromJson(Map<String, dynamic> json) {
    final identity = json['identity'];
    String? email;
    String? plan;
    if (identity is Map<String, dynamic>) {
      final rawEmail = identity['email'] as String?;
      if (rawEmail != null && rawEmail.trim().isNotEmpty) {
        email = rawEmail;
      }
      final rawPlan = identity['plan'] as String?;
      if (rawPlan != null && rawPlan.trim().isNotEmpty) {
        plan = rawPlan;
      }
    }
    final quotaRaw = json['quota'];
    return ProviderAccountSummary(
      id: json['id']?.toString() ?? '',
      label: json['label'] as String? ?? '',
      authKind: json['auth_kind'] as String? ?? '',
      status: json['status'] as String? ?? '',
      email: email,
      plan: plan,
      quota: quotaRaw is Map<String, dynamic>
          ? ProviderQuotaSnapshot.fromJson(quotaRaw)
          : null,
    );
  }

  final String id;
  final String label;
  final String authKind;
  final String status;
  final String? email;
  final String? plan;
  final ProviderQuotaSnapshot? quota;

  ProviderAccountSummary copyWithQuota(ProviderQuotaSnapshot? nextQuota) {
    return ProviderAccountSummary(
      id: id,
      label: label,
      authKind: authKind,
      status: status,
      email: email,
      plan: plan,
      quota: nextQuota,
    );
  }
}

class AiProviderSummary {
  const AiProviderSummary({
    required this.id,
    required this.kind,
    required this.auth,
    required this.status,
    required this.health,
    this.accounts = const [],
  });

  factory AiProviderSummary.fromJson(Map<String, dynamic> json) {
    final rawAccounts = json['accounts'] as List<dynamic>? ?? const [];
    return AiProviderSummary(
      id: json['id']?.toString() ?? '',
      kind: json['kind'] as String? ?? '',
      auth: json['auth'] as String? ?? '',
      status: json['status'] as String? ?? '',
      health: json['health'] as String? ?? '',
      accounts: rawAccounts
          .whereType<Map<String, dynamic>>()
          .map(ProviderAccountSummary.fromJson)
          .toList(),
    );
  }

  final String id;
  final String kind;
  final String auth;
  final String status;
  final String health;
  final List<ProviderAccountSummary> accounts;
}
