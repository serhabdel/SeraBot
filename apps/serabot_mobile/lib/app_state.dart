import 'dart:async';

import 'package:flutter/foundation.dart';

import 'api_client.dart';
import 'chat_models.dart';
import 'models.dart';

/// Per-user data shared by every screen: the conversation list (same
/// `GET /v1/conversations` as the desktop, same order), bots, the pending
/// approval count and the `GET /v1/events` stream that keeps them current.
class AppState extends ChangeNotifier {
  AppState(this.client, {this.eventDebounce = const Duration(milliseconds: 400)});

  final ApiClient client;
  final Duration eventDebounce;

  List<ConversationSummary> conversations = const [];
  List<BotSummary> bots = const [];
  List<ApprovalItem> approvals = const [];
  int pendingApprovals = 0;
  bool loadingConversations = false;
  String? conversationsError;
  bool live = false;

  final _events = StreamController<ServerEvent>.broadcast();
  Stream<ServerEvent> get events => _events.stream;

  int _cursor = 0;
  int get cursor => _cursor;
  bool _disposed = false;
  bool _running = false;
  Timer? _debounce;
  Timer? _approvalDebounce;

  int get unread => totalUnread(conversations);

  /// Start the event loop and the first loads. Safe to call once.
  void start({bool events = true}) {
    if (_running) {
      return;
    }
    _running = true;
    refreshConversations();
    refreshBots();
    refreshApprovals();
    if (events) {
      _loop();
    }
  }

  Future<void> refreshConversations() async {
    loadingConversations = true;
    _notify();
    try {
      conversations = await client.conversations();
      conversationsError = null;
    } on ApiException catch (e) {
      conversationsError = e.messageForUser();
    } finally {
      loadingConversations = false;
      _notify();
    }
  }

  Future<void> refreshBots() async {
    try {
      bots = await client.bots();
      _notify();
    } on ApiException {
      // The list screen shows its own error on a manual refresh.
    }
  }

  Future<void> refreshApprovals() async {
    try {
      approvals = await client.approvals();
      pendingApprovals = approvals.length;
      _notify();
    } on ApiException {
      // Keep the last known count.
    }
  }

  /// Opening a thread marks it read on the server; mirror that locally.
  void markReadLocally(String id) {
    final i = conversations.indexWhere((c) => c.id == id);
    if (i >= 0 && conversations[i].unreadCount > 0) {
      final next = [...conversations];
      next[i] = next[i].copyWith(unreadCount: 0);
      conversations = next;
      _notify();
    }
  }

  final _avatars = <String, Future<Uint8List?>>{};
  final _avatarBytes = <String, Uint8List?>{};

  String? _avatarKey(String kind, String id, String? version) {
    final v = version?.trim() ?? '';
    if (v.isEmpty) {
      return kind == 'bots' ? '$kind/$id/default' : null;
    }
    return '$kind/$id/$v';
  }

  /// Synchronous in-memory avatar bytes so `SeraAvatar` renders on frame 0
  /// without a `FutureBuilder` flash.
  Uint8List? cachedAvatar(String kind, String id, String? version) {
    final key = _avatarKey(kind, id, version);
    if (key == null) {
      return null;
    }
    return _avatarBytes[key];
  }

  /// Bot (`bots`) or group (`conversations`) picture, cached per version.
  /// Fetches `/v1/bots/{id}/avatar` even when [version] is null/empty since
  /// the server serves default avatars for shipped bots.
  Future<Uint8List?> avatar(String kind, String id, String? version) {
    final key = _avatarKey(kind, id, version);
    if (key == null) {
      return Future.value(null);
    }
    return _avatars.putIfAbsent(key, () async {
      try {
        final b = await client.avatarBytes(kind, id);
        final bytes = b == null ? null : Uint8List.fromList(b);
        _avatarBytes[key] = bytes;
        return bytes;
      } on ApiException {
        _avatarBytes[key] = null;
        return null;
      }
    });
  }

  BotSummary? botById(String? id) {
    if (id == null) {
      return null;
    }
    for (final b in bots) {
      if (b.id == id) {
        return b;
      }
    }
    return null;
  }

  /// Applies one server event. Public for tests.
  void handleEvent(ServerEvent e) {
    if (e.id > _cursor) {
      _cursor = e.id;
    }
    _events.add(e);
    final k = e.kind;
    if (k == 'message' ||
        k == 'conversation.created' ||
        k == 'conversation.renamed' ||
        k == 'turn.completed' ||
        k == 'task.failed' ||
        k == 'handoff.queued' ||
        k == 'brief.delivered') {
      _debounce?.cancel();
      _debounce = Timer(eventDebounce, refreshConversations);
    }
    if (k == 'approval' || k.startsWith('approval.') || k == 'turn.step') {
      if (k != 'turn.step' || e.payload['approval_id'] != null) {
        _approvalDebounce?.cancel();
        _approvalDebounce = Timer(eventDebounce, refreshApprovals);
      }
    }
  }

  Future<void> _loop() async {
    var attempt = 0;
    const backoff = [1, 2, 5, 10, 30];
    while (!_disposed) {
      try {
        await for (final e in client.events(after: _cursor)) {
          if (_disposed) {
            return;
          }
          if (!live) {
            live = true;
            _notify();
          }
          attempt = 0;
          handleEvent(e);
        }
      } on ApiException catch (e) {
        if (e.kind == ApiErrorKind.http && e.statusCode == 401) {
          return;
        }
      } catch (_) {
        // fall through to reconnect
      }
      if (_disposed) {
        return;
      }
      if (live) {
        live = false;
        _notify();
      }
      await Future<void>.delayed(
        Duration(seconds: backoff[attempt.clamp(0, backoff.length - 1)]),
      );
      attempt++;
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
    _debounce?.cancel();
    _approvalDebounce?.cancel();
    _events.close();
    super.dispose();
  }
}
