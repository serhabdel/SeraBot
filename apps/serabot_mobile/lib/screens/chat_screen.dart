import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../chat_controller.dart';
import '../chat_models.dart';
import '../chat_scroll.dart';
import '../device.dart';
import '../markdown.dart';
import '../models.dart';
import '../preview_pane.dart';
import '../widgets.dart';
import 'trace_sheet.dart';

class ChatScreen extends StatefulWidget {
  const ChatScreen({
    super.key,
    required this.app,
    required this.conversation,
    required this.device,
  });

  final AppState app;
  final ConversationSummary conversation;
  final DeviceActions device;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  late final ChatController chat;
  final _scroll = ScrollController();
  bool _atBottom = true;
  int _unseen = 0;
  int _lastCount = 0;

  @override
  void initState() {
    super.initState();
    chat = ChatController(widget.conversation.id, widget.app.client, widget.app)
      ..load();
    _scroll.addListener(_onScroll);
    chat.addListener(_onChat);
  }

  @override
  void dispose() {
    chat.removeListener(_onChat);
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    chat.dispose();
    super.dispose();
  }

  void _onScroll() {
    if (!_scroll.hasClients || !mounted) {
      return;
    }
    final near = nearBottom(_scroll.position.pixels);
    if (near != _atBottom || (near && _unseen != 0)) {
      setState(() {
        _atBottom = near;
        if (near) {
          _unseen = 0;
        }
      });
    }
  }

  void _onChat() {
    final count = chat.messages.length;
    final unseen = nextUnseen(
      unseen: _unseen,
      previousCount: _lastCount,
      count: count,
      atBottom: _atBottom,
    );
    _lastCount = count;
    if (unseen != _unseen && mounted) {
      setState(() => _unseen = unseen);
    }
  }

  void _jumpToLatest() {
    if (_scroll.hasClients) {
      final duration = motionDuration(context, const Duration(milliseconds: 250));
      if (duration == Duration.zero) {
        _scroll.jumpTo(0);
      } else {
        _scroll.animateTo(0, duration: duration, curve: Curves.easeOut);
      }
    }
    setState(() {
      _atBottom = true;
      _unseen = 0;
    });
  }

  Future<void> _download(String name, Future<List<int>> Function() fetch) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      final bytes = await fetch();
      await widget.device.shareFile(name, bytes);
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    } catch (_) {
      messenger.showSnackBar(const SnackBar(content: Text('Could not open the file.')));
    }
  }

  Future<void> _modelSheet() async {
    await chat.loadCatalog();
    if (!mounted) {
      return;
    }
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (_) => ModelSheet(chat: chat),
    );
  }

  void _filesSheet() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (_) => ListenableBuilder(
        listenable: chat,
        builder: (context, _) => FilesSheet(
          entries: chat.library,
          onOpen: (e) => _download(
            e.label,
            () => widget.app.client.artifactBytes(e.hash),
          ),
          // 0.28.0: documents the server can render open in a preview pane;
          // the share action stays as the fallback.
          onPreview: (e) => showArtifactPreviewSheet(
            context,
            client: widget.app.client,
            entry: e,
            onFallback: () {
              Navigator.pop(context);
              _download(e.label, () => widget.app.client.artifactBytes(e.hash));
            },
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.conversation;
    return ListenableBuilder(
      listenable: chat,
      builder: (context, _) {
        final sel = chat.catalog?.selection;
        return Scaffold(
          appBar: AppBar(
            titleSpacing: 0,
            title: Row(children: [
              SeraAvatar(
                label: c.displayTitle,
                app: widget.app,
                kind: 'conversations',
                id: c.id,
                version: c.avatarVersion,
                radius: 16,
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(c.displayTitle, maxLines: 1, overflow: TextOverflow.ellipsis),
                    if (chat.running)
                      Text('Working\u2026',
                          style: Theme.of(context).textTheme.bodySmall),
                  ],
                ),
              ),
            ]),
            actions: [
              ActionChip(
                key: const Key('model-chip'),
                avatar: const Icon(Icons.auto_awesome, size: 16),
                label: Text(sel == null ? 'Model' : (sel.isAuto ? 'Auto' : sel.model),
                    overflow: TextOverflow.ellipsis),
                onPressed: _modelSheet,
              ),
              IconButton(
                tooltip: 'Files',
                onPressed: _filesSheet,
                icon: Badge(
                  isLabelVisible: chat.library.isNotEmpty,
                  label: Text('${chat.library.length}'),
                  child: const Icon(Icons.folder_open),
                ),
              ),
              IconButton(
                tooltip: 'Tool trace',
                onPressed: () => showConversationTraceSheet(
                  context,
                  client: widget.app.client,
                  conversationId: widget.conversation.id,
                  title: c.displayTitle,
                ),
                icon: const Icon(Icons.timeline_outlined),
              ),
            ],
          ),
          body: Column(
            children: [
              Expanded(
                child: Stack(
                  children: [
                    _messages(context),
                    if (!_atBottom)
                      Positioned(
                        left: 0,
                        right: 0,
                        bottom: 12,
                        child: Center(
                          child: _JumpToLatestPill(
                            badge: unseenBadge(_unseen),
                            replying: chat.running,
                            onPressed: _jumpToLatest,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              if (chat.error != null)
                Padding(
                  padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
                  child: ErrorBanner(chat.error!, onRetry: chat.load),
                ),
              Composer(chat: chat, device: widget.device),
            ],
          ),
        );
      },
    );
  }

  Widget _messages(BuildContext context) {
    if (chat.loading && chat.messages.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    final items = chat.messages.reversed.toList();
    final showActivity = chat.running || !chat.activity.isEmpty;
    return ListView.builder(
      controller: _scroll,
      reverse: true,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      itemCount: items.length + (showActivity ? 1 : 0),
      itemBuilder: (context, i) {
        if (showActivity && i == 0) {
          return ActivityPanel(chat: chat);
        }
        final m = items[i - (showActivity ? 1 : 0)];
        return MessageBubble(
          message: m,
          app: widget.app,
          onAttachment: (a) => _download(
            a.filename,
            () => widget.app.client.attachmentBytes(a.id),
          ),
        );
      },
    );
  }
}

/// "Jump to latest" with an unseen count and a marker while a reply is still
/// coming in. The marker is static when the system asks for reduced motion.
class _JumpToLatestPill extends StatelessWidget {
  const _JumpToLatestPill({
    required this.badge,
    required this.replying,
    required this.onPressed,
  });

  final String? badge;
  final bool replying;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final reduced = MediaQuery.disableAnimationsOf(context);
    final label = badge == null ? 'Jump to latest' : 'Jump to latest \u00b7 $badge';
    return FilledButton.tonalIcon(
      key: const Key('jump-to-latest'),
      onPressed: onPressed,
      icon: const Icon(Icons.arrow_downward, size: 18),
      label: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(label),
          if (replying) ...[
            const SizedBox(width: 8),
            if (reduced)
              const Icon(Icons.more_horiz, size: 16)
            else
              const SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
          ],
        ],
      ),
    );
  }
}

class MessageBubble extends StatelessWidget {
  const MessageBubble({
    super.key,
    required this.message,
    required this.app,
    this.onAttachment,
  });

  final ChatMessage message;
  final AppState app;
  final void Function(AttachmentInfo)? onAttachment;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final mine = message.role == MessageRole.user;
    final bot = app.botById(message.speaker);
    final bubble = Container(
      constraints: BoxConstraints(maxWidth: MediaQuery.of(context).size.width * 0.82),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
      decoration: BoxDecoration(
        color: mine ? scheme.primaryContainer : scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.only(
          topLeft: const Radius.circular(18),
          topRight: const Radius.circular(18),
          bottomLeft: Radius.circular(mine ? 18 : 4),
          bottomRight: Radius.circular(mine ? 4 : 18),
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (!mine && message.speaker != null)
            Padding(
              padding: const EdgeInsets.only(bottom: 4),
              child: Text(
                bot?.name ?? message.speaker!,
                style: Theme.of(context)
                    .textTheme
                    .labelMedium
                    ?.copyWith(color: scheme.primary),
              ),
            ),
          if (message.content.isNotEmpty)
            mine
                ? SelectableText(message.content)
                : MarkdownText(message.content),
          for (final a in message.attachments)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: ActionChip(
                avatar: Icon(a.isImage ? Icons.image_outlined : Icons.attach_file, size: 16),
                label: Text(a.filename, overflow: TextOverflow.ellipsis),
                onPressed: onAttachment == null ? null : () => onAttachment!(a),
              ),
            ),
          _BubbleFooter(message: message),
        ],
      ),
    );
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        mainAxisAlignment: mine ? MainAxisAlignment.end : MainAxisAlignment.start,
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          if (!mine) ...[
            SeraAvatar(
              label: bot?.name ?? message.speaker ?? 'S',
              emoji: bot?.avatar,
              app: app,
              id: bot?.id,
              version: bot?.avatarVersion,
              radius: 14,
            ),
            const SizedBox(width: 6),
          ],
          Flexible(child: bubble),
        ],
      ),
    );
  }
}

/// Send time and a Copy action under a message, like the desktop bubbles.
class _BubbleFooter extends StatelessWidget {
  const _BubbleFooter({required this.message});

  final ChatMessage message;

  @override
  Widget build(BuildContext context) {
    final time = shortTime(message.createdAt);
    final canCopy = message.content.isNotEmpty;
    if (time.isEmpty && !canCopy) {
      return const SizedBox.shrink();
    }
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    return Padding(
      padding: const EdgeInsets.only(top: 4),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (time.isNotEmpty)
            Text(time,
                key: const Key('bubble-time'),
                style: Theme.of(context)
                    .textTheme
                    .labelSmall
                    ?.copyWith(color: muted)),
          if (canCopy)
            IconButton(
              key: const Key('copy-message'),
              tooltip: 'Copy message',
              visualDensity: VisualDensity.compact,
              iconSize: 16,
              color: muted,
              icon: const Icon(Icons.copy),
              onPressed: () async {
                final messenger = ScaffoldMessenger.of(context);
                await Clipboard.setData(ClipboardData(text: message.content));
                messenger.showSnackBar(
                  const SnackBar(content: Text('Copied')),
                );
              },
            ),
        ],
      ),
    );
  }
}

/// Live steps of the running turn, collapsed by default.
class ActivityPanel extends StatelessWidget {
  const ActivityPanel({super.key, required this.chat});
  final ChatController chat;

  IconData _icon(StepPhase p) => switch (p) {
        StepPhase.running => Icons.autorenew,
        StepPhase.done => Icons.check_circle_outline,
        StepPhase.failed => Icons.error_outline,
        StepPhase.waitingApproval => Icons.gavel,
      };

  @override
  Widget build(BuildContext context) {
    final a = chat.activity;
    final waiting = a.steps.where((s) => s.phase == StepPhase.waitingApproval);
    final title = chat.running
        ? (a.isEmpty ? 'Working\u2026' : 'Working \u00b7 ${a.summary}')
        : a.summary;
    return Card(
      margin: const EdgeInsets.symmetric(vertical: 6),
      child: ExpansionTile(
        key: const Key('activity-panel'),
        leading: chat.running
            ? const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : const Icon(Icons.checklist),
        title: Text(title),
        initiallyExpanded: waiting.isNotEmpty,
        children: [
          for (final s in a.steps)
            ListTile(
              dense: true,
              leading: Icon(_icon(s.phase), size: 18),
              title: Text(s.label),
              trailing: s.phase == StepPhase.waitingApproval && s.approvalId != null
                  ? Row(mainAxisSize: MainAxisSize.min, children: [
                      TextButton(
                        onPressed: () => chat.decide(s.approvalId!, ApprovalDecision.deny),
                        child: const Text('Deny'),
                      ),
                      FilledButton(
                        onPressed: () => chat.decide(s.approvalId!, ApprovalDecision.once),
                        child: const Text('Approve'),
                      ),
                    ])
                  : null,
            ),
        ],
      ),
    );
  }
}

/// Text box with attach, microphone and send/stop.
class Composer extends StatefulWidget {
  const Composer({super.key, required this.chat, required this.device});
  final ChatController chat;
  final DeviceActions device;

  @override
  State<Composer> createState() => _ComposerState();
}

class _ComposerState extends State<Composer> {
  final _text = TextEditingController();
  bool _recording = false;
  bool _transcribing = false;

  ChatController get chat => widget.chat;

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final text = _text.text;
    final ok = await chat.send(text);
    if (ok) {
      _text.clear();
    }
  }

  Future<void> _attach({required bool images}) async {
    try {
      final files = await widget.device.pickFiles(imagesOnly: images);
      for (final f in files) {
        await chat.addAttachment(f.name, f.bytes);
      }
    } catch (_) {
      // Picker cancelled or unavailable.
    }
  }

  void _attachMenu() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => SafeArea(
        child: Column(mainAxisSize: MainAxisSize.min, children: [
          ListTile(
            leading: const Icon(Icons.image_outlined),
            title: const Text('Photo'),
            onTap: () {
              Navigator.pop(ctx);
              _attach(images: true);
            },
          ),
          ListTile(
            leading: const Icon(Icons.attach_file),
            title: const Text('File'),
            subtitle: const Text('PDF, Office, text, CSV or image, up to 25 MB'),
            onTap: () {
              Navigator.pop(ctx);
              _attach(images: false);
            },
          ),
        ]),
      ),
    );
  }

  Future<void> _toggleMic() async {
    final messenger = ScaffoldMessenger.of(context);
    if (!_recording) {
      final ok = await widget.device.startRecording();
      if (!ok) {
        messenger.showSnackBar(
          const SnackBar(content: Text('Allow microphone access to record a voice note.')),
        );
        return;
      }
      setState(() => _recording = true);
      return;
    }
    setState(() {
      _recording = false;
      _transcribing = true;
    });
    try {
      final audio = await widget.device.stopRecording();
      if (audio != null) {
        final text = await chat.client.transcribe(audio.name, audio.bytes);
        if (text.trim().isNotEmpty) {
          final sep = _text.text.isEmpty ? '' : ' ';
          _text.text = '${_text.text}$sep${text.trim()}';
          _text.selection = TextSelection.collapsed(offset: _text.text.length);
        }
      }
    } on ApiException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
    } finally {
      if (mounted) {
        setState(() => _transcribing = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([chat, _text]),
      builder: (context, _) {
        final canSend = !chat.sending &&
            !chat.running &&
            !chat.uploading &&
            (_text.text.trim().isNotEmpty || chat.attachments.isNotEmpty);
        return SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(8, 4, 8, 8),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (chat.attachments.isNotEmpty || chat.uploading)
                  Align(
                    alignment: Alignment.centerLeft,
                    child: Wrap(spacing: 6, children: [
                      for (final a in chat.attachments)
                        InputChip(
                          label: Text(a.filename, overflow: TextOverflow.ellipsis),
                          onDeleted: () => chat.removeAttachment(a),
                        ),
                      if (chat.uploading)
                        const Chip(label: Text('Uploading\u2026')),
                    ]),
                  ),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    IconButton(
                      key: const Key('attach-button'),
                      tooltip: 'Attach',
                      onPressed: chat.canAddAttachment ? _attachMenu : null,
                      icon: const Icon(Icons.add_circle_outline),
                    ),
                    Expanded(
                      child: TextField(
                        key: const Key('composer-field'),
                        controller: _text,
                        minLines: 1,
                        maxLines: 6,
                        maxLength: 8000,
                        textCapitalization: TextCapitalization.sentences,
                        decoration: InputDecoration(
                          hintText: _recording ? 'Recording\u2026' : 'Message',
                          counterText: '',
                          isDense: true,
                          filled: true,
                        ),
                        onChanged: (_) {},
                      ),
                    ),
                    IconButton(
                      key: const Key('mic-button'),
                      tooltip: _recording ? 'Stop and transcribe' : 'Voice note',
                      onPressed: _transcribing ? null : _toggleMic,
                      color: _recording ? Theme.of(context).colorScheme.error : null,
                      icon: _transcribing
                          ? const SizedBox(
                              width: 20,
                              height: 20,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : Icon(_recording ? Icons.stop_circle : Icons.mic_none),
                    ),
                    chat.running
                        ? IconButton.filledTonal(
                            key: const Key('stop-button'),
                            tooltip: 'Stop',
                            onPressed: chat.stop,
                            icon: const Icon(Icons.stop),
                          )
                        : IconButton.filled(
                            key: const Key('send-button'),
                            tooltip: 'Send',
                            onPressed: canSend ? _send : null,
                            icon: const Icon(Icons.arrow_upward),
                          ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

/// Model picker. Server refusals (for example a provider without data-egress
/// approval) are shown with the server's own text.
class ModelSheet extends StatefulWidget {
  const ModelSheet({super.key, required this.chat});
  final ChatController chat;

  @override
  State<ModelSheet> createState() => _ModelSheetState();
}

class _ModelSheetState extends State<ModelSheet> {
  String? _error;
  bool _busy = false;

  Future<void> _pick(Selection s) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.chat.selectModel(s);
    if (!mounted) {
      return;
    }
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _error = err;
        _busy = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final cat = widget.chat.catalog;
    if (cat == null) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: widget.chat.error != null
            ? ErrorBanner(widget.chat.error!)
            : const Center(child: CircularProgressIndicator()),
      );
    }
    final current = cat.selection;
    return SafeArea(
      child: ListView(
        shrinkWrap: true,
        children: [
          if (_error != null)
            Padding(padding: const EdgeInsets.all(12), child: ErrorBanner(_error!)),
          ListTile(
            leading: Icon(current.isAuto ? Icons.radio_button_checked : Icons.radio_button_off),
            title: const Text('Auto'),
            subtitle: const Text('The server chooses per turn'),
            onTap: _busy ? null : () => _pick(Selection.auto()),
          ),
          for (final p in cat.providers)
            ExpansionTile(
              title: Text(p.id),
              subtitle: p.dataEgressApproved ? null : const Text('Needs data-egress approval'),
              initiallyExpanded: p.id == current.provider,
              children: [
                for (final m in p.models)
                  ListTile(
                    dense: true,
                    leading: Icon(
                      p.id == current.provider && m == current.model
                          ? Icons.radio_button_checked
                          : Icons.radio_button_off,
                    ),
                    title: Text(m),
                    onTap: _busy
                        ? null
                        : () => _pick(Selection(provider: p.id, model: m)),
                  ),
              ],
            ),
        ],
      ),
    );
  }
}

class FilesSheet extends StatelessWidget {
  const FilesSheet({
    super.key,
    required this.entries,
    required this.onOpen,
    this.onPreview,
  });
  final List<LibraryEntry> entries;
  final void Function(LibraryEntry) onOpen;
  /// 0.28.0: opens the server-rendered preview for pdf/pptx/xlsx/docx/md.
  final void Function(LibraryEntry)? onPreview;

  @override
  Widget build(BuildContext context) {
    if (entries.isEmpty) {
      return const Padding(
        padding: EdgeInsets.all(32),
        child: Center(child: Text('No files produced in this chat yet.')),
      );
    }
    return SafeArea(
      child: ListView(
        shrinkWrap: true,
        children: [
          for (final e in entries)
            ListTile(
              leading: const Icon(Icons.insert_drive_file_outlined),
              title: Text(e.label, maxLines: 1, overflow: TextOverflow.ellipsis),
              subtitle: Text(shortTime(e.createdAt)),
              trailing: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  // Preview only for the formats the server can render.
                  if (onPreview != null && previewKindForName(e.label) != null)
                    IconButton(
                      tooltip: 'Preview',
                      icon: const Icon(Icons.visibility_outlined),
                      onPressed: () => onPreview!(e),
                    ),
                  IconButton(
                    tooltip: 'Open / share',
                    icon: const Icon(Icons.share_outlined),
                    onPressed: () => onOpen(e),
                  ),
                ],
              ),
              onTap: () => onOpen(e),
            ),
        ],
      ),
    );
  }
}
