import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import 'api_client.dart';
import 'app_state.dart';
import 'chat_models.dart';
import 'models.dart';
import 'turn_memory.dart';

/// Most messages one conversation's queue holds, like the desktop.
const maxQueuedMessages = 8;

/// Most characters one message may carry, matching the server's limit.
const maxMessageChars = 8000;

/// Most characters one persisted draft keeps.
const maxPersistedDraftChars = 32768;

/// Most conversations whose drafts are kept on disk.
const maxPersistedDraftConversations = 64;

/// The `@mention` query being typed at the end of [text], if any: the last
/// `@` that starts the text or follows whitespace, whose tail has no
/// whitespace, is at most 32 chars and matches `[A-Za-z0-9-_]*`. A bare `@`
/// yields `''`. The tail is kept as typed; matching lowercases it.
String? mentionQuery(String text) {
  final at = text.lastIndexOf('@');
  if (at < 0) {
    return null;
  }
  if (at > 0 && !_isWhitespace(text[at - 1])) {
    return null;
  }
  final tail = text.substring(at + 1);
  if (tail.length > 32 || tail.contains(RegExp(r'\s'))) {
    return null;
  }
  if (!RegExp(r'^[A-Za-z0-9\-_]*$').hasMatch(tail)) {
    return null;
  }
  return tail;
}

bool _isWhitespace(String ch) =>
    ch == ' ' || ch == '\n' || ch == '\t' || ch == '\r';

/// The `/command` query at the start of the last line of [text], if the rest
/// of that line has no whitespace. A bare `/` yields `''`.
String? slashQuery(String text) {
  final line = text.split('\n').last;
  if (!line.startsWith('/')) {
    return null;
  }
  final rest = line.substring(1);
  if (rest.contains(RegExp(r'\s'))) {
    return null;
  }
  return rest;
}

/// A slash action the mobile composer can run from the picker.
class MobileSlashAction {
  const MobileSlashAction(this.command, this.description);
  final String command;
  final String description;
}

/// Slash actions the mobile composer offers, in picker order.
const mobileSlashActions = [
  MobileSlashAction('attach', 'Attach files'),
  MobileSlashAction('stop', 'Stop the reply'),
];

/// The mobile slash actions whose command contains [query]
/// (case-insensitive).
List<MobileSlashAction> mobileSlashMatches(String query) {
  final needle = query.trim().toLowerCase();
  return mobileSlashActions
      .where((a) => needle.isEmpty || a.command.contains(needle))
      .toList();
}

/// A message waiting for the running reply to end.
class QueuedMessage {
  const QueuedMessage(this.text, {this.attachmentIds = const []});
  final String text;

  /// Ids of files already uploaded for this message.
  final List<String> attachmentIds;
}

/// Drafts saved on this device, one per conversation, as a small JSON file
/// in the app documents directory. Text only; never secrets.
class DraftStore {
  DraftStore({Future<Directory> Function()? directory})
    : _directory = directory ?? getApplicationDocumentsDirectory;

  final Future<Directory> Function() _directory;
  static const _fileName = 'serabot_drafts.json';

  Future<File> _file() async {
    final dir = await _directory();
    return File('${dir.path}/$_fileName');
  }

  /// All saved drafts, or empty when nothing was saved yet.
  Future<Map<String, String>> loadAll() async {
    try {
      final raw = await (await _file()).readAsString();
      final decoded = jsonDecode(raw);
      if (decoded is! Map<String, dynamic>) {
        return {};
      }
      final drafts = <String, String>{};
      for (final entry in decoded.entries) {
        final text = entry.value;
        if (text is String &&
            text.trim().isNotEmpty &&
            text.length <= maxPersistedDraftChars) {
          drafts[entry.key] = text;
        }
        if (drafts.length >= maxPersistedDraftConversations) {
          break;
        }
      }
      return drafts;
    } catch (_) {
      return {};
    }
  }

  /// Save [drafts], atomically where the platform allows it.
  Future<void> saveAll(Map<String, String> drafts) async {
    try {
      final file = await _file();
      final clean = <String, String>{};
      for (final entry in drafts.entries) {
        if (entry.value.trim().isEmpty) {
          continue;
        }
        clean[entry.key] = entry.value.length > maxPersistedDraftChars
            ? entry.value.substring(0, maxPersistedDraftChars)
            : entry.value;
        if (clean.length >= maxPersistedDraftConversations) {
          break;
        }
      }
      await file.writeAsString(jsonEncode(clean), flush: true);
    } catch (_) {
      // Drafts are best effort; the chat works without them.
    }
  }
}

/// State of one open conversation: the message list, the running turn with
/// its live steps, and the composer's pending attachments.
///
/// Replies arrive the way the desktop gets them: the turn is queued with
/// `POST /v1/messages`, `turn.step` events fill the activity list, and
/// `turn.completed` / `task.failed` (or the task poll, as a fallback when the
/// event stream is down) triggers a reload of the transcript.
class ChatController extends ChangeNotifier {
  ChatController(
    this.conversationId,
    this.client,
    this.app, {
    this.pollInterval = const Duration(seconds: 2),
    this.livePollInterval = const Duration(seconds: 20),
    this.draftStore,
  }) {
    _sub = app.events.listen(_onEvent);
  }

  final String conversationId;
  final ApiClient client;
  final AppState app;

  /// Saves drafts on this device when set; `null` keeps drafts in memory
  /// only (tests and callers that do not want disk writes).
  final DraftStore? draftStore;

  /// Task poll gap while the event stream is down.
  final Duration pollInterval;

  /// Safety poll gap while the event stream is up: events finish the turn, the
  /// poll only covers a lost event.
  final Duration livePollInterval;

  List<ChatMessage> messages = const [];
  final Activity activity = Activity();

  /// Memories each turn used and saved (`turn.memory`), by task id. Bounded to
  /// the most recent turns; a reply's memory shows next to its steps.
  final Map<String, TurnMemory> _memoryByTask = {};
  static const _maxMemoryTurns = 32;

  /// The task of the last reply this screen started or replayed.
  String? _lastTaskId;

  /// Memory of one turn, or an empty memory when none was recorded.
  TurnMemory memoryFor(String? taskId) {
    if (taskId == null) {
      return const TurnMemory();
    }
    return _memoryByTask[taskId] ?? const TurnMemory();
  }

  /// Memory of the running turn, or of the last reply when none is running.
  TurnMemory get currentMemory => memoryFor(runningTaskId ?? _lastTaskId);

  final List<AttachmentInfo> attachments = [];
  List<LibraryEntry> library = const [];
  Catalog? catalog;

  bool loading = true;
  bool uploading = false;
  bool sending = false;
  String? error;
  ChatAction errorAction = ChatAction.none;

  /// True when [error] is from a send, so Retry sends [draft] again.
  bool sendFailed = false;

  /// Composer text, kept so a failed send can be retried from the banner.
  String draft = '';
  int acceptedSends = 0;
  ChatPlan? plan;
  String? runningTaskId;

  /// Elapsed seconds reported by the latest `turn.progress` heartbeat for the
  /// running turn, or null before the first beat / once a turn finishes.
  int? progressElapsedSecs;

  /// True when the active task entered `needs_review` (uncertain effect; do
  /// not auto-retry or show a spinner). Cleared by [dismissNeedsReview] or
  /// starting a new turn.
  bool needsReview = false;

  Timer? _poll;
  DateTime _lastPoll = DateTime.fromMillisecondsSinceEpoch(0);
  StreamSubscription<ServerEvent>? _sub;
  bool _disposed = false;

  bool get running => runningTaskId != null && !needsReview;
  bool get canAddAttachment => attachments.length < maxAttachments;

  /// Dismisses the `needs_review` banner once the owner has checked the result.
  void dismissNeedsReview() {
    if (!needsReview) {
      return;
    }
    needsReview = false;
    runningTaskId = null;
    progressElapsedSecs = null;
    _poll?.cancel();
    _notify();
  }

  /// Messages typed while a reply runs, in order. They go out one at a time
  /// when the chat is idle.
  final List<QueuedMessage> queued = [];

  /// True once [restoreDraft] has read the saved draft for this chat.
  bool draftLoaded = false;

  void noteDraft(String text) {
    draft = text;
    _scheduleDraftSave();
  }

  Timer? _draftSaveTimer;

  void _scheduleDraftSave() {
    final store = draftStore;
    if (store == null) {
      return;
    }
    _draftSaveTimer?.cancel();
    _draftSaveTimer = Timer(const Duration(milliseconds: 500), () async {
      final drafts = await store.loadAll();
      if (draft.trim().isEmpty) {
        drafts.remove(conversationId);
      } else {
        drafts[conversationId] = draft;
      }
      await store.saveAll(drafts);
    });
  }

  /// Read the saved draft for this chat into [draft]. A no-op without a
  /// [draftStore], and a saved draft never overwrites text already typed.
  Future<void> restoreDraft() async {
    final store = draftStore;
    if (store == null || draftLoaded) {
      draftLoaded = true;
      return;
    }
    final drafts = await store.loadAll();
    final saved = drafts[conversationId];
    if (saved != null && saved.isNotEmpty && draft.trim().isEmpty) {
      draft = saved;
    }
    draftLoaded = true;
    _notify();
  }

  /// Queue a turn behind the running reply. Files already uploaded travel
  /// with it. Returns `false` for a blank or over-long message, or when the
  /// queue is full.
  Future<bool> queueSend(String text) async {
    final trimmed = text.trim();
    if (trimmed.isEmpty || trimmed.length > maxMessageChars) {
      return false;
    }
    if (queued.length >= maxQueuedMessages) {
      return false;
    }
    queued.add(
      QueuedMessage(
        trimmed,
        attachmentIds: attachments.map((a) => a.id).toList(),
      ),
    );
    attachments.clear();
    draft = '';
    _scheduleDraftSave();
    _notify();
    return true;
  }

  void removeQueued(int index) {
    if (index < 0 || index >= queued.length) {
      return;
    }
    queued.removeAt(index);
    _notify();
  }

  /// Send the first queued message now that the chat is idle. Does nothing
  /// while a reply runs or the queue is empty.
  Future<void> pumpQueue() async {
    if (queued.isEmpty || running || sending) {
      return;
    }
    final next = queued.removeAt(0);
    _notify();
    await send(next.text, attachmentIds: next.attachmentIds);
  }

  void _fail(ApiException e, {bool send = false}) {
    if (send) {
      final notice = classifySendError(
        kind: e.kind.name,
        status: e.statusCode,
        detail: e.detail ?? e.messageForUser(),
      );
      error = notice.message;
      errorAction = notice.action;
      sendFailed = true;
    } else {
      error = e.messageForUser();
      errorAction = ChatAction.retry;
      sendFailed = false;
    }
  }

  Future<void> load() async {
    loading = true;
    _notify();
    try {
      messages = await client.history(conversationId);
      error = null;
      app.markReadLocally(conversationId);
    } on ApiException catch (e) {
      _fail(e);
    } finally {
      loading = false;
      _notify();
    }
    unawaited(loadLibrary());
    unawaited(loadPlan());
    unawaited(loadMemory());
  }

  /// Replays the stored memory of recent replies, so the last reply's memories
  /// show after the chat is reopened. A live event for a turn always wins.
  Future<void> loadMemory() async {
    try {
      final events = await client.conversationMemoryEvents(conversationId);
      final replayed = <String, TurnMemory>{};
      String? newest;
      for (final e in events) {
        final taskId = e.taskId;
        if (taskId == null || taskId.isEmpty) {
          continue;
        }
        replayed[taskId] = (replayed[taskId] ?? const TurnMemory()).applyEvent(
          e.payload,
        );
        newest = taskId;
      }
      for (final entry in replayed.entries) {
        _memoryByTask.putIfAbsent(entry.key, () => entry.value);
      }
      _lastTaskId ??= newest;
      _trimMemory();
      if (events.isNotEmpty) {
        _notify();
      }
    } on ApiException {
      // The memory panel is optional; the chat works without it.
    }
  }

  void _trimMemory() {
    while (_memoryByTask.length > _maxMemoryTurns) {
      _memoryByTask.remove(_memoryByTask.keys.first);
    }
  }

  Future<void> loadPlan() async {
    try {
      plan = await client.conversationPlan(conversationId);
      _notify();
    } on ApiException {
      // Older servers have no plan route. The checklist stays hidden.
    }
  }

  Future<void> loadLibrary() async {
    try {
      library = await client.library(conversationId);
      _notify();
    } on ApiException {
      // Optional panel; chat keeps working without it.
    }
  }

  Future<void> loadCatalog() async {
    try {
      catalog = await client.models(conversationId);
      _notify();
    } on ApiException catch (e) {
      _fail(e);
      _notify();
    }
  }

  /// Change the conversation's model. Returns the server's error sentence on
  /// failure so the UI can show it as is.
  Future<String?> selectModel(Selection selection) async {
    try {
      await client.selectModel(conversationId, selection);
      await loadCatalog();
      return null;
    } on ApiException catch (e) {
      return e.messageForUser();
    }
  }

  Future<void> addAttachment(String filename, List<int> bytes) async {
    if (!canAddAttachment) {
      error = 'Attach at most $maxAttachments files.';
      errorAction = ChatAction.none;
      sendFailed = false;
      _notify();
      return;
    }
    final name = safeUploadName(filename);
    uploading = true;
    error = null;
    _notify();
    try {
      attachments.add(
        await client.uploadAttachment(conversationId, name, bytes),
      );
    } on ApiException catch (e) {
      _fail(e);
    } finally {
      uploading = false;
      _notify();
    }
  }

  void removeAttachment(AttachmentInfo a) {
    attachments.remove(a);
    _notify();
  }

  /// Queue a turn. The text may be empty when files are attached. While a
  /// reply runs the turn waits in [queued] instead of being refused.
  Future<bool> send(String text, {List<String>? attachmentIds}) async {
    if (sending) {
      return false;
    }
    if (running) {
      return queueSend(text);
    }
    final trimmed = text.trim();
    final ids = attachmentIds ?? attachments.map((a) => a.id).toList();
    if (trimmed.isEmpty && ids.isEmpty) {
      return false;
    }
    sending = true;
    error = null;
    sendFailed = false;
    _notify();
    try {
      final task = await client.submit(
        conversationId,
        trimmed,
        attachments: ids,
      );
      messages = [
        ...messages,
        ChatMessage(
          id: 'local-${DateTime.now().microsecondsSinceEpoch}',
          role: MessageRole.user,
          content: trimmed,
          createdAt: DateTime.now().toUtc().toIso8601String(),
          attachments: List.of(attachments),
        ),
      ];
      attachments.clear();
      activity.clear();
      runningTaskId = task.id;
      progressElapsedSecs = null;
      needsReview = false;
      _lastTaskId = task.id;
      acceptedSends++;
      draft = '';
      _startPoll();
      return true;
    } on ApiException catch (e) {
      _fail(e, send: true);
      return false;
    } finally {
      sending = false;
      _notify();
    }
  }

  Future<void> stop() async {
    try {
      await client.cancelTurn(conversationId);
    } on ApiException catch (e) {
      _fail(e);
      _notify();
    }
  }

  /// Approve or deny from the live activity list.
  Future<void> decide(String approvalId, ApprovalDecision d) async {
    try {
      await client.decideApproval(approvalId, d);
      await app.refreshApprovals();
    } on ApiException catch (e) {
      _fail(e);
      _notify();
    }
  }

  void _onEvent(ServerEvent e) {
    if (_disposed) {
      return;
    }
    if (e.conversationId != null && e.conversationId != conversationId) {
      return;
    }
    switch (e.kind) {
      case 'turn.step':
        final plan = ChatPlan.parse(
          e.payload['plan'] is Map<String, dynamic>
              ? e.payload['plan'] as Map<String, dynamic>
              : null,
        );
        if (plan != null) {
          this.plan = plan;
          _notify();
        }
        // Only the turn this screen started; old replayed events are ignored.
        if (runningTaskId != null && e.taskId == runningTaskId) {
          if (activity.apply(e.kind, e.payload)) {
            _notify();
          }
        }
      case 'turn.progress':
        if (runningTaskId != null && e.taskId == runningTaskId) {
          final raw = e.payload['elapsed_secs'];
          final secs = raw is num && raw.isFinite && raw >= 0
              ? raw.toInt().clamp(0, 86400)
              : null;
          if (secs != null && secs != progressElapsedSecs) {
            progressElapsedSecs = secs;
            _notify();
          }
        }
      case 'turn.completed':
      case 'task.failed':
        if (runningTaskId == null || e.taskId == runningTaskId) {
          final statusRaw = e.payload['status'];
          if (e.kind == 'task.failed' && statusRaw == 'needs_review') {
            _markNeedsReview();
          } else if (e.kind == 'task.failed' && runningTaskId != null) {
            unawaited(_checkNeedsReviewOnFailure(
              runningTaskId!,
              detail: _errorOf(e),
            ));
          } else {
            _finish(failed: e.kind == 'task.failed', detail: _errorOf(e));
          }
        }
      case 'turn.memory':
        final taskId = e.taskId;
        if (taskId != null && taskId.isNotEmpty) {
          // Re-insert so the map keeps the most recent turns last.
          final next = memoryFor(taskId).applyEvent(e.payload);
          _memoryByTask.remove(taskId);
          _memoryByTask[taskId] = next;
          _trimMemory();
          if (taskId == (runningTaskId ?? _lastTaskId)) {
            _notify();
          }
        }
      case 'plan.updated':
        final next = ChatPlan.parse(e.payload);
        if (next != null) {
          plan = next;
          _notify();
        }
      case 'message':
        if (e.conversationId == conversationId && !running) {
          unawaited(_reload());
        }
      case 'resume':
        // Back from the background: events may have been missed.
        if (running) {
          unawaited(_pollTask(force: true));
        } else {
          unawaited(_reload());
        }
    }
  }

  Future<void> _checkNeedsReviewOnFailure(
    String taskId, {
    String? detail,
  }) async {
    try {
      final t = await client.task(taskId);
      if (_disposed || runningTaskId != taskId) {
        return;
      }
      if (t.status == TaskStatus.needsReview) {
        _markNeedsReview();
        return;
      }
    } on ApiException {
      // Fall back to standard failure handling when the task lookup fails
      // (e.g. older server or stub without GET /v1/tasks/{id}).
    }
    if (!_disposed && runningTaskId == taskId) {
      await _finish(failed: true, detail: detail);
    }
  }

  void _markNeedsReview() {
    if (_disposed || needsReview) {
      return;
    }
    needsReview = true;
    progressElapsedSecs = null;
    _poll?.cancel();
    _notify();
    unawaited(_reload());
    unawaited(loadLibrary());
  }

  String? _errorOf(ServerEvent e) {
    final v = e.payload['error'];
    return v is String && v.isNotEmpty && v.length <= 300 ? v : null;
  }

  /// Starts the task poll without a send. For tests.
  @visibleForTesting
  void startPollForTest() => _startPoll();

  void _startPoll() {
    _poll?.cancel();
    _lastPoll = DateTime.now();
    _poll = Timer.periodic(pollInterval, (_) => _pollTask());
  }

  /// One task check. Skipped in the background, and while the event stream is
  /// live it only runs as a slow safety net. [force] is for a resume.
  Future<void> _pollTask({bool force = false}) async {
    final id = runningTaskId;
    if (id == null || needsReview || _disposed) {
      return;
    }
    if (!force) {
      if (!app.foreground) {
        return;
      }
      final gap = app.live ? livePollInterval : pollInterval;
      if (DateTime.now().difference(_lastPoll) < gap) {
        return;
      }
    }
    _lastPoll = DateTime.now();
    try {
      final t = await client.task(id);
      if (runningTaskId != id || _disposed) {
        return;
      }
      if (t.status == TaskStatus.needsReview) {
        _markNeedsReview();
      } else if (t.status.terminal) {
        _finish(failed: t.status == TaskStatus.failed);
      }
    } on ApiException {
      // transient; try again next tick
    }
  }

  Future<void> _finish({bool failed = false, String? detail}) async {
    if (runningTaskId == null) {
      return;
    }
    runningTaskId = null;
    progressElapsedSecs = null;
    _poll?.cancel();
    if (failed) {
      final text =
          detail ?? 'The turn failed. The steps above say what went wrong.';
      error = presentError(text);
      errorAction = ChatAction.none;
      sendFailed = false;
    }
    _notify();
    await _reload();
    unawaited(loadLibrary());
    unawaited(pumpQueue());
  }

  Future<void> _reload() async {
    try {
      final next = await client.history(conversationId);
      // Keep the list the screen already shows when nothing changed, so a
      // reload that finds the same rows does not rebuild the transcript.
      if (!sameMessages(messages, next)) {
        messages = next;
        _notify();
      }
    } on ApiException catch (e) {
      _fail(e);
      _notify();
    }
  }

  void _notify() {
    if (!_disposed) {
      notifyListeners();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _draftSaveTimer?.cancel();
    _poll?.cancel();
    _sub?.cancel();
    super.dispose();
  }
}

/// True when [a] and [b] show the same rows in the same order.
bool sameMessages(List<ChatMessage> a, List<ChatMessage> b) {
  if (a.length != b.length) {
    return false;
  }
  for (var i = 0; i < a.length; i++) {
    if (a[i].id != b[i].id ||
        a[i].content != b[i].content ||
        a[i].attachments.length != b[i].attachments.length) {
      return false;
    }
  }
  return true;
}
