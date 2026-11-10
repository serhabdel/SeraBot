// Wave C5 / A4: read-only Workflows + traces parsing (mirrors serabot-desktop workflows.rs).

const _maxName = 120;
const _maxTime = 64;
const _maxGoal = 500;
const _maxError = 300;
const _maxUrl = 512;
const _maxKind = 64;
const _maxConversation = 128;
const _maxRoutines = 64;
const _maxSources = 200;
const _maxActive = 200;
const _maxTraces = 500;
const _maxTool = 128;
const _maxEvent = 64;
const _maxHash = 128;
const _maxDest = 512;

bool _isControl(int r) => r <= 0x1f || (r >= 0x7f && r <= 0x9f);

String? _clean(dynamic value, String key, int max) {
  if (value is! Map<String, dynamic>) {
    return null;
  }
  final raw = value[key];
  if (raw is! String) {
    return null;
  }
  final text = raw.runes
      .where((r) => !_isControl(r))
      .take(max)
      .map(String.fromCharCode)
      .join()
      .trim();
  return text.isEmpty ? null : text;
}

String? _optStr(Map<String, dynamic> value, String key, int max) {
  final v = value[key];
  if (v == null) {
    return null;
  }
  return _clean(value, key, max);
}

String _cleanOrEmpty(Map<String, dynamic> value, String key, int max) {
  return _clean(value, key, max) ?? '';
}

class Routine {
  const Routine({
    required this.name,
    required this.enabled,
    required this.nextRunAt,
    this.lastRunFor,
    this.time,
    this.timezone,
  });

  final String name;
  final bool enabled;
  final String nextRunAt;
  final String? lastRunFor;
  final String? time;
  final String? timezone;

  static Routine? fromJson(Map<String, dynamic> row) {
    final name = _clean(row, 'name', _maxName);
    final enabled = row['enabled'];
    if (name == null || enabled is! bool) {
      return null;
    }
    final config = row['config'];
    final cfg = config is Map<String, dynamic> ? config : const <String, dynamic>{};
    String? time;
    final t = cfg['time'];
    if (t is String && t.isNotEmpty) {
      time = t.runes.take(16).map(String.fromCharCode).join();
    }
    String? timezone;
    final tz = cfg['timezone'] ?? cfg['tz'];
    if (tz is String && tz.isNotEmpty) {
      timezone = tz.runes.take(64).map(String.fromCharCode).join();
    }
    return Routine(
      name: name,
      enabled: enabled,
      nextRunAt: _cleanOrEmpty(row, 'next_run_at', _maxTime),
      lastRunFor: _optStr(row, 'last_run_for', _maxTime),
      time: time,
      timezone: timezone,
    );
  }
}

class SourceLastRun {
  const SourceLastRun({
    required this.ok,
    this.error,
    required this.itemsSeen,
    this.startedAt,
    this.finishedAt,
  });

  final bool ok;
  final String? error;
  final int itemsSeen;
  final String? startedAt;
  final String? finishedAt;

  static SourceLastRun? fromJson(Map<String, dynamic> row) {
    final ok = row['ok'];
    if (ok is! bool) {
      return null;
    }
    final items = row['items_seen'];
    return SourceLastRun(
      ok: ok,
      error: _optStr(row, 'error', _maxError),
      itemsSeen: items is int ? items : (items is num ? items.toInt() : 0),
      startedAt: _optStr(row, 'started_at', _maxTime),
      finishedAt: _optStr(row, 'finished_at', _maxTime),
    );
  }
}

class MonitoringSource {
  const MonitoringSource({
    required this.name,
    required this.kind,
    required this.enabled,
    this.url,
    this.lastRun,
  });

  final String name;
  final String kind;
  final bool enabled;
  final String? url;
  final SourceLastRun? lastRun;

  static MonitoringSource? fromJson(Map<String, dynamic> row) {
    final name = _clean(row, 'name', _maxName);
    if (name == null) {
      return null;
    }
    final kind = _clean(row, 'kind', _maxKind) ?? 'unknown';
    final enabled = row['enabled'] is bool ? row['enabled'] as bool : true;
    final lastRaw = row['last_run'];
    final lastRun = lastRaw is Map<String, dynamic>
        ? SourceLastRun.fromJson(lastRaw)
        : null;
    return MonitoringSource(
      name: name,
      kind: kind,
      enabled: enabled,
      url: _optStr(row, 'url', _maxUrl),
      lastRun: lastRun,
    );
  }
}

class ActiveWorkflow {
  const ActiveWorkflow({
    required this.conversationId,
    required this.goal,
    required this.done,
    required this.total,
    this.updatedAt,
  });

  final String conversationId;
  final String goal;
  final int done;
  final int total;
  final String? updatedAt;

  static ActiveWorkflow? fromJson(Map<String, dynamic> row) {
    final conversationId = _clean(row, 'conversation_id', _maxConversation);
    if (conversationId == null) {
      return null;
    }
    final doneRaw = row['done'];
    final totalRaw = row['total'];
    final done = doneRaw is int
        ? doneRaw
        : (doneRaw is num ? doneRaw.toInt() : 0);
    final total = totalRaw is int
        ? totalRaw
        : (totalRaw is num ? totalRaw.toInt() : 0);
    return ActiveWorkflow(
      conversationId: conversationId,
      goal: _clean(row, 'goal', _maxGoal) ?? '(no goal)',
      done: done,
      total: total,
      updatedAt: _optStr(row, 'updated_at', _maxTime),
    );
  }
}

class WorkflowsSnapshot {
  const WorkflowsSnapshot({
    this.routines = const [],
    this.sources = const [],
    this.active = const [],
    this.routinesSupported = false,
    this.sourcesSupported = false,
    this.activeSupported = false,
  });

  final List<Routine> routines;
  final List<MonitoringSource> sources;
  final List<ActiveWorkflow> active;
  final bool routinesSupported;
  final bool sourcesSupported;
  final bool activeSupported;
}

List<Routine>? parseRoutines(Map<String, dynamic> value) {
  final rows = value['routines'];
  if (rows is! List || rows.length > _maxRoutines) {
    return null;
  }
  final out = <Routine>[];
  for (final row in rows) {
    if (row is! Map<String, dynamic>) {
      return null;
    }
    final r = Routine.fromJson(row);
    if (r == null) {
      continue;
    }
    out.add(r);
  }
  return out;
}

List<MonitoringSource>? parseSources(Map<String, dynamic> value) {
  final rows = value['sources'];
  if (rows is! List || rows.length > _maxSources) {
    return null;
  }
  final out = <MonitoringSource>[];
  for (final row in rows) {
    if (row is! Map<String, dynamic>) {
      return null;
    }
    final s = MonitoringSource.fromJson(row);
    if (s == null) {
      continue;
    }
    out.add(s);
  }
  return out;
}

List<ActiveWorkflow>? parseActiveWorkflows(Map<String, dynamic> value) {
  final rows = value['workflows'];
  if (rows is! List || rows.length > _maxActive) {
    return null;
  }
  final out = <ActiveWorkflow>[];
  for (final row in rows) {
    if (row is! Map<String, dynamic>) {
      return null;
    }
    final w = ActiveWorkflow.fromJson(row);
    if (w == null) {
      continue;
    }
    out.add(w);
  }
  return out;
}

String formatNextRun(String raw) {
  final flat = raw.runes
      .where((r) => !_isControl(r))
      .take(_maxTime)
      .map(String.fromCharCode)
      .join()
      .trim();
  return flat.isEmpty ? '\u2014' : flat;
}

String planProgress(int done, int total) {
  if (total == 0) {
    return '0 steps';
  }
  return '$done/$total';
}

class ConversationTrace {
  const ConversationTrace({
    required this.id,
    required this.at,
    required this.event,
    this.tool,
    this.approvalId,
    this.actionId,
    this.canonicalHash,
    this.destination,
    this.elapsedMs,
    this.artifactHash,
  });

  final int id;
  final String at;
  final String event;
  final String? tool;
  final String? approvalId;
  final String? actionId;
  final String? canonicalHash;
  final String? destination;
  final int? elapsedMs;
  final String? artifactHash;

  static ConversationTrace? fromJson(Map<String, dynamic> row) {
    final id = row['id'];
    if (id is! num) {
      return null;
    }
    final event = _clean(row, 'event', _maxEvent);
    final at = _clean(row, 'at', _maxTime);
    if (event == null || at == null) {
      return null;
    }
    final elapsed = row['elapsed_ms'];
    int? elapsedMs;
    if (elapsed is int) {
      elapsedMs = elapsed;
    } else if (elapsed is num) {
      elapsedMs = elapsed.toInt();
    }
    return ConversationTrace(
      id: id.toInt(),
      at: at,
      event: event,
      tool: _optStr(row, 'tool', _maxTool),
      approvalId: _optStr(row, 'approval_id', _maxHash),
      actionId: _optStr(row, 'action_id', _maxHash),
      canonicalHash: _optStr(row, 'canonical_hash', _maxHash),
      destination: _optStr(row, 'destination', _maxDest),
      elapsedMs: elapsedMs,
      artifactHash: _optStr(row, 'artifact_hash', _maxHash),
    );
  }
}

ConversationTracePage parseTraces(Map<String, dynamic> value) {
  final conversationId = _clean(value, 'conversation_id', _maxConversation) ?? '';
  final rows = value['traces'];
  if (rows is! List || rows.length > _maxTraces) {
    throw const FormatException('invalid traces');
  }
  final traces = <ConversationTrace>[];
  for (final row in rows) {
    if (row is! Map<String, dynamic>) {
      throw const FormatException('invalid trace row');
    }
    final t = ConversationTrace.fromJson(row);
    if (t == null) {
      continue;
    }
    traces.add(t);
  }
  return ConversationTracePage(conversationId: conversationId, traces: traces);
}

class ConversationTracePage {
  const ConversationTracePage({
    required this.conversationId,
    required this.traces,
  });

  final String conversationId;
  final List<ConversationTrace> traces;
}
