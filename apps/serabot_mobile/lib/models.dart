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
  });

  factory ChatMessage.fromJson(Map<String, dynamic> json) {
    return ChatMessage(
      id: json['id'] as String,
      role: parseRole(json['role'] as String),
      content: json['content'] as String,
      createdAt: json['created_at'] as String,
    );
  }

  final String id;
  final MessageRole role;
  final String content;
  final String createdAt;
}

enum TaskStatus {
  queued,
  running,
  awaitingApproval,
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
  });

  factory ApprovalItem.fromJson(Map<String, dynamic> json) {
    return ApprovalItem(
      id: json['id'] as String,
      actionId: json['action_id'] as String,
      tool: json['tool'] as String,
      summary: json['summary'] as String,
      expiresAt: json['expires_at'] as String,
    );
  }

  final String id;
  final String actionId;
  final String tool;
  final String summary;
  final String expiresAt;
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
