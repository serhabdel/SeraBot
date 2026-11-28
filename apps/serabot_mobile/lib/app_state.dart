import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import 'api_client.dart';
import 'chat_models.dart';
import 'models.dart';

const _defaultIdle = eventIdleTimeout;

/// Per-user data shared by every screen: the conversation list (same
/// `GET /v1/conversations` as the desktop, same order), bots, the pending
/// approval count and the `GET /v1/events` stream that keeps them current.
/// The stream follows the app lifecycle: it closes when the app goes to the
/// background and reconnects, with a catch-up load, when it returns. A change
/// of network ([networkChanges]) reconnects at once instead of waiting out the
/// backoff.
class AppState extends ChangeNotifier with WidgetsBindingObserver {
  AppState(
    this.client, {
    this.eventDebounce = const Duration(milliseconds: 400),
    this.networkChanges,
    this.eventIdleTimeout = _defaultIdle,
  });

  final ApiClient client;
  final Duration eventDebounce;

  /// Fires when the device's network changes (Wi-Fi to cellular, back online).
  final Stream<void>? networkChanges;

  /// Silence on the event stream that counts as a dead connection.
  final Duration eventIdleTimeout;

  List<ConversationSummary> conversations = const [];
  List<BotSummary> bots = const [];
  List<ApprovalItem> approvals = const [];
  int pendingApprovals = 0;
  bool loadingConversations = false;
  String? conversationsError;
  bool live = false;
  bool seenLive = false;

  final _events = StreamController<ServerEvent>.broadcast();
  Stream<ServerEvent> get events => _events.stream;

  /// Emits the number of approvals that appeared since the last check. The
  /// first load only learns what is already waiting and does not alert.
  final _approvalAlerts = StreamController<int>.broadcast();
  Stream<int> get approvalAlerts => _approvalAlerts.stream;
  final Set<String> _knownApprovals = {};
  bool _approvalsLoaded = false;

  int _cursor = 0;
  int get cursor => _cursor;
  bool _disposed = false;
  bool _running = false;
  bool _paused = false;
  bool _observing = false;
  StreamSubscription<void>? _networkSub;
  StreamSubscription<ServerEvent>? _eventSub;
  Completer<void>? _wake;
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
      WidgetsBinding.instance.addObserver(this);
      _observing = true;
      _networkSub = networkChanges?.listen((_) => reconnectNow());
      _loop();
    }
  }

  /// True while the app is in the foreground.
  bool get foreground => !_paused;

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        resume();
      case AppLifecycleState.paused:
      case AppLifecycleState.hidden:
      case AppLifecycleState.detached:
        pause();
      case AppLifecycleState.inactive:
        break;
    }
  }

  /// Background: close the stream so a sleeping phone holds no socket and no
  /// timers. The cursor is kept for the catch-up on [resume].
  void pause() {
    if (_paused) {
      return;
    }
    _paused = true;
    _debounce?.cancel();
    _approvalDebounce?.cancel();
    _eventSub?.cancel();
    _eventSub = null;
    _poke();
    if (live) {
      live = false;
      _notify();
    }
  }

  /// Foreground again: reconnect now with a fresh backoff, then reload what
  /// may have changed while the stream was closed.
  void resume() {
    if (!_paused) {
      return;
    }
    _paused = false;
    _resetBackoff = true;
    _poke();
    refreshConversations();
    refreshApprovals();
    // Open chats reload their transcript and task state on this synthetic event.
    if (!_events.isClosed) {
      _events.add(const ServerEvent(0, 'resume', {}));
    }
  }

  /// The network changed: drop the current connection and dial again now.
  void reconnectNow() {
    if (_paused || _disposed) {
      return;
    }
    _eventSub?.cancel();
    _eventSub = null;
    _resetBackoff = true;
    _poke();
  }

  bool _resetBackoff = false;

  void _poke() {
    final wake = _wake;
    if (wake != null && !wake.isCompleted) {
      wake.complete();
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
      final next = await client.approvals();
      approvals = next;
      pendingApprovals = next.length;
      _noteApprovals(next);
      _notify();
    } on ApiException {
      // Keep the last known count.
    }
  }

  void _noteApprovals(List<ApprovalItem> items) {
    final ids = {for (final item in items) item.id};
    if (!_approvalsLoaded) {
      _approvalsLoaded = true;
      _knownApprovals
        ..clear()
        ..addAll(ids);
      return;
    }
    final fresh = ids.difference(_knownApprovals).length;
    _knownApprovals
      ..clear()
      ..addAll(ids);
    if (fresh > 0 && !_approvalAlerts.isClosed) {
      _approvalAlerts.add(fresh);
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

  /// Waits up to [d], or until [_poke] (pause, resume, network change, stop).
  Future<void> _sleep(Duration d) async {
    final wake = Completer<void>();
    _wake = wake;
    final timer = Timer(d, () {
      if (!wake.isCompleted) {
        wake.complete();
      }
    });
    await wake.future;
    timer.cancel();
  }

  Future<void> _loop() async {
    var attempt = 0;
    const backoff = [1, 2, 5, 10, 30];
    while (!_disposed) {
      if (_paused) {
        await _sleep(const Duration(hours: 1));
        continue;
      }
      if (_resetBackoff) {
        _resetBackoff = false;
        attempt = 0;
      }
      final wake = Completer<void>();
      _wake = wake;
      var unauthorized = false;
      _eventSub = client
          .events(
            after: _cursor,
            idleTimeout: eventIdleTimeout,
            onOpen: () {
              // HTTP 200: the link is up even if the stream is quiet.
              attempt = 0;
              seenLive = true;
              if (!live) {
                live = true;
                _notify();
              }
            },
          )
          .listen(
            (e) {
              if (!_disposed) {
                handleEvent(e);
              }
            },
            onError: (Object e) {
              if (e is ApiException &&
                  e.kind == ApiErrorKind.http &&
                  e.statusCode == 401) {
                unauthorized = true;
              }
              if (!wake.isCompleted) {
                wake.complete();
              }
            },
            onDone: () {
              if (!wake.isCompleted) {
                wake.complete();
              }
            },
            cancelOnError: true,
          );
      await wake.future;
      await _eventSub?.cancel();
      _eventSub = null;
      if (_disposed || unauthorized) {
        return;
      }
      if (live) {
        live = false;
        _notify();
      }
      if (_paused || _resetBackoff) {
        continue;
      }
      await _sleep(
        jittered(Duration(seconds: backoff[attempt.clamp(0, backoff.length - 1)])),
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
    if (_observing) {
      WidgetsBinding.instance.removeObserver(this);
    }
    _networkSub?.cancel();
    _eventSub?.cancel();
    _poke();
    _debounce?.cancel();
    _approvalDebounce?.cancel();
    _events.close();
    _approvalAlerts.close();
    super.dispose();
  }
}
