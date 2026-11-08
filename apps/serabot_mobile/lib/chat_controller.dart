import 'dart:async';

import 'package:flutter/foundation.dart';

import 'api_client.dart';
import 'app_state.dart';
import 'chat_models.dart';
import 'models.dart';

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
  }) {
    _sub = app.events.listen(_onEvent);
  }

  final String conversationId;
  final ApiClient client;
  final AppState app;
  final Duration pollInterval;

  List<ChatMessage> messages = const [];
  final Activity activity = Activity();
  final List<AttachmentInfo> attachments = [];
  List<LibraryEntry> library = const [];
  Catalog? catalog;

  bool loading = true;
  bool uploading = false;
  bool sending = false;
  String? error;
  String? runningTaskId;
  Timer? _poll;
  StreamSubscription<ServerEvent>? _sub;
  bool _disposed = false;

  bool get running => runningTaskId != null;
  bool get canAddAttachment => attachments.length < maxAttachments;

  Future<void> load() async {
    loading = true;
    _notify();
    try {
      messages = await client.history(conversationId);
      error = null;
      app.markReadLocally(conversationId);
    } on ApiException catch (e) {
      error = e.messageForUser();
    } finally {
      loading = false;
      _notify();
    }
    unawaited(loadLibrary());
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
      error = e.messageForUser();
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
      error = e.messageForUser();
    } finally {
      uploading = false;
      _notify();
    }
  }

  void removeAttachment(AttachmentInfo a) {
    attachments.remove(a);
    _notify();
  }

  /// Queue a turn. The text may be empty when files are attached.
  Future<bool> send(String text) async {
    if (sending || running) {
      return false;
    }
    final trimmed = text.trim();
    if (trimmed.isEmpty && attachments.isEmpty) {
      return false;
    }
    sending = true;
    error = null;
    _notify();
    try {
      final ids = attachments.map((a) => a.id).toList();
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
      _startPoll();
      return true;
    } on ApiException catch (e) {
      error = e.messageForUser();
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
      error = e.messageForUser();
      _notify();
    }
  }

  /// Approve or deny from the live activity list.
  Future<void> decide(String approvalId, ApprovalDecision d) async {
    try {
      await client.decideApproval(approvalId, d);
      await app.refreshApprovals();
    } on ApiException catch (e) {
      error = e.messageForUser();
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
        // Only the turn this screen started; old replayed events are ignored.
        if (runningTaskId != null && e.taskId == runningTaskId) {
          if (activity.apply(e.kind, e.payload)) {
            _notify();
          }
        }
      case 'turn.completed':
      case 'task.failed':
        if (runningTaskId == null || e.taskId == runningTaskId) {
          _finish(failed: e.kind == 'task.failed', detail: _errorOf(e));
        }
      case 'message':
        if (e.conversationId == conversationId && !running) {
          unawaited(_reload());
        }
    }
  }

  String? _errorOf(ServerEvent e) {
    final v = e.payload['error'];
    return v is String && v.isNotEmpty && v.length <= 300 ? v : null;
  }

  void _startPoll() {
    _poll?.cancel();
    _poll = Timer.periodic(pollInterval, (_) async {
      final id = runningTaskId;
      if (id == null || _disposed) {
        return;
      }
      try {
        final t = await client.task(id);
        if (t.status.terminal) {
          _finish(failed: t.status == TaskStatus.failed);
        }
      } on ApiException {
        // transient; try again next tick
      }
    });
  }

  Future<void> _finish({bool failed = false, String? detail}) async {
    if (runningTaskId == null) {
      return;
    }
    runningTaskId = null;
    _poll?.cancel();
    if (failed) {
      error = detail ?? 'The turn failed. Check the server log.';
    }
    _notify();
    await _reload();
    unawaited(loadLibrary());
  }

  Future<void> _reload() async {
    try {
      messages = await client.history(conversationId);
      _notify();
    } on ApiException catch (e) {
      error = e.messageForUser();
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
    _poll?.cancel();
    _sub?.cancel();
    super.dispose();
  }
}
