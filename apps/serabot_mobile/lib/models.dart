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
