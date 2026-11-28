import 'package:material_ui/material_ui.dart';

import 'app_state.dart';
import 'chat_models.dart';
import 'device.dart';
import 'screens/alerts_screen.dart';
import 'screens/bots_screen.dart';
import 'screens/chat_screen.dart';
import 'screens/chats_screen.dart';
import 'screens/costs_screen.dart';
import 'screens/jobs_screen.dart';
import 'screens/mail_screen.dart';
import 'screens/usage_insights_screen.dart';

bool _isWordChar(int codeUnit) {
  return (codeUnit >= 0x30 && codeUnit <= 0x39) ||
      (codeUnit >= 0x41 && codeUnit <= 0x5A) ||
      (codeUnit >= 0x61 && codeUnit <= 0x7A);
}

int? _scoreSingleTerm(String termLower, String hayLower) {
  if (termLower.isEmpty) return 0;
  if (termLower.length > hayLower.length) return null;

  final subAt = hayLower.indexOf(termLower);
  if (subAt >= 0) {
    final atBoundary =
        subAt == 0 || !_isWordChar(hayLower.codeUnitAt(subAt - 1));
    return 1000 + (atBoundary ? 200 : 0) - subAt;
  }

  var score = 0;
  var pos = 0;
  int? prev;
  for (var i = 0; i < termLower.length; i++) {
    final want = termLower.codeUnitAt(i);
    var found = -1;
    for (var j = pos; j < hayLower.length; j++) {
      if (hayLower.codeUnitAt(j) == want) {
        found = j;
        break;
      }
    }
    if (found < 0) return null;

    score += 10;
    if (found == 0 || !_isWordChar(hayLower.codeUnitAt(found - 1))) {
      score += 15;
    }
    if (prev != null) {
      if (found == prev + 1) {
        score += 12;
      } else {
        final gap = found - prev - 1;
        score -= gap > 8 ? 8 : gap;
      }
    } else {
      score -= found > 10 ? 10 : found;
    }
    prev = found;
    pos = found + 1;
  }
  return score;
}

List<int> _matchIndicesSingleTerm(String termLower, String hayLower) {
  if (termLower.isEmpty || termLower.length > hayLower.length) {
    return const [];
  }
  final subAt = hayLower.indexOf(termLower);
  if (subAt >= 0) {
    return [for (var i = subAt; i < subAt + termLower.length; i++) i];
  }
  final out = <int>[];
  var pos = 0;
  for (var i = 0; i < termLower.length; i++) {
    final want = termLower.codeUnitAt(i);
    var found = -1;
    for (var j = pos; j < hayLower.length; j++) {
      if (hayLower.codeUnitAt(j) == want) {
        found = j;
        break;
      }
    }
    if (found < 0) return const [];
    out.add(found);
    pos = found + 1;
  }
  return out;
}

/// Scores how well [query] matches [text] using case-insensitive subsequence
/// and multi-term matching. Returns `null` when any whitespace-separated term
/// fails to match [text] in order; returns `0` for an empty query.
int? fuzzyMatchScore(String query, String text) {
  final terms = query
      .trim()
      .toLowerCase()
      .split(RegExp(r'\s+'))
      .where((t) => t.isNotEmpty)
      .toList();
  if (terms.isEmpty) return 0;

  final hayLower = text.toLowerCase();
  var total = 0;
  for (final term in terms) {
    final s = _scoreSingleTerm(term, hayLower);
    if (s == null) return null;
    total += s;
  }
  return total;
}

/// Returns merged, sorted `[start, end)` highlight ranges in [text] for
/// [query], preferring contiguous substring matches per term.
List<({int start, int end})> fuzzyMatchRanges(String query, String text) {
  final terms = query
      .trim()
      .toLowerCase()
      .split(RegExp(r'\s+'))
      .where((t) => t.isNotEmpty)
      .toList();
  if (terms.isEmpty || text.isEmpty) return const [];

  final hayLower = text.toLowerCase();
  if (hayLower.length != text.length) {
    // Fallback if lowercasing changes UTF-16 length: avoid out-of-bounds.
    return const [];
  }

  final indices = <int>{};
  for (final term in terms) {
    final matched = _matchIndicesSingleTerm(term, hayLower);
    if (matched.isEmpty) return const [];
    indices.addAll(matched);
  }
  if (indices.isEmpty) return const [];

  final sorted = indices.toList()..sort();
  final ranges = <({int start, int end})>[];
  var start = sorted.first;
  var end = start + 1;
  for (var i = 1; i < sorted.length; i++) {
    final idx = sorted[i];
    if (idx == end) {
      end++;
    } else {
      ranges.add((start: start, end: end));
      start = idx;
      end = idx + 1;
    }
  }
  ranges.add((start: start, end: end));
  return ranges;
}

class _JumpEntry {
  const _JumpEntry({
    required this.id,
    required this.title,
    required this.subtitle,
    required this.searchText,
    required this.icon,
    required this.onSelect,
  });

  final String id;
  final String title;
  final String subtitle;
  final String searchText;
  final IconData icon;
  final VoidCallback onSelect;
}

/// Opens the Spotlight-style Quick Jump modal bottom sheet.
Future<void> showQuickJumpSheet(
  BuildContext context, {
  required AppState app,
  required DeviceActions device,
  ValueChanged<int>? onSelectTab,
  ValueChanged<BotSummary>? onOpenBot,
  ValueChanged<ConversationSummary>? onOpenConversation,
}) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    useSafeArea: true,
    builder: (_) => QuickJumpSheet(
      app: app,
      device: device,
      onSelectTab: onSelectTab,
      onOpenBot: onOpenBot,
      onOpenConversation: onOpenConversation,
    ),
  );
}

/// Spotlight-style Quick Jump modal bottom sheet indexing main tabs,
/// sub-screens, active bots, and recent conversations.
class QuickJumpSheet extends StatefulWidget {
  const QuickJumpSheet({
    super.key,
    required this.app,
    required this.device,
    this.onSelectTab,
    this.onOpenBot,
    this.onOpenConversation,
  });

  final AppState app;
  final DeviceActions device;
  final ValueChanged<int>? onSelectTab;
  final ValueChanged<BotSummary>? onOpenBot;
  final ValueChanged<ConversationSummary>? onOpenConversation;

  @override
  State<QuickJumpSheet> createState() => _QuickJumpSheetState();
}

class _QuickJumpSheetState extends State<QuickJumpSheet> {
  final _query = TextEditingController();

  @override
  void dispose() {
    _query.dispose();
    super.dispose();
  }

  void _pushScreen(Widget screen) {
    final nav = Navigator.of(context);
    nav.pop();
    nav.push(MaterialPageRoute<void>(builder: (_) => screen));
  }

  List<_JumpEntry> _buildEntries() {
    final entries = <_JumpEntry>[];

    void addTab(int index, String label, IconData icon) {
      entries.add(_JumpEntry(
        id: 'tab-$index',
        title: label,
        subtitle: 'Tab',
        searchText: '$label tab',
        icon: icon,
        onSelect: () {
          Navigator.of(context).pop();
          widget.onSelectTab?.call(index);
        },
      ));
    }

    if (widget.onSelectTab != null) {
      addTab(0, 'Chats', Icons.chat_bubble_outline_rounded);
      addTab(1, 'Tasks', Icons.check_box_outlined);
      addTab(2, 'Workflows', Icons.account_tree_outlined);
      addTab(3, 'Library', Icons.folder_outlined);
      addTab(4, 'Bots', Icons.smart_toy_outlined);
      addTab(5, 'Settings', Icons.settings_outlined);
    }

    entries.addAll([
      _JumpEntry(
        id: 'screen-mail',
        title: 'Mail',
        subtitle: 'Tool · SMTP / IMAP inbox',
        searchText: 'Mail inbox email smtp imap',
        icon: Icons.mail_outlined,
        onSelect: () => _pushScreen(MailScreen(app: widget.app)),
      ),
      _JumpEntry(
        id: 'screen-costs',
        title: 'Costs',
        subtitle: 'Tool · Monthly budget & plans',
        searchText: 'Costs budget spending plans',
        icon: Icons.payments_outlined,
        onSelect: () => _pushScreen(CostsScreen(app: widget.app)),
      ),
      _JumpEntry(
        id: 'screen-insights',
        title: 'Usage Insights',
        subtitle: 'Tool · Channels, tunnel & model speed',
        searchText: 'Usage Insights telemetry speed tunnel',
        icon: Icons.insights_outlined,
        onSelect: () => _pushScreen(UsageInsightsScreen(app: widget.app)),
      ),
      _JumpEntry(
        id: 'screen-alerts',
        title: 'Alerts',
        subtitle: 'Tool · Telegram, WhatsApp & brief',
        searchText: 'Alerts delivery telegram whatsapp brief',
        icon: Icons.notifications_outlined,
        onSelect: () => _pushScreen(AlertsScreen(app: widget.app)),
      ),
      _JumpEntry(
        id: 'screen-jobs',
        title: 'Jobs',
        subtitle: 'Tool · Job search & applications',
        searchText: 'Jobs search applications matches',
        icon: Icons.work_outline,
        onSelect: () => _pushScreen(JobsScreen(app: widget.app)),
      ),
    ]);

    for (final bot in widget.app.bots.where((b) => b.enabled)) {
      entries.add(_JumpEntry(
        id: 'bot-${bot.id}',
        title: bot.name,
        subtitle: 'Bot · ${bot.id}',
        searchText: '${bot.name} ${bot.id} ${bot.description ?? ''} bot',
        icon: Icons.smart_toy_outlined,
        onSelect: () {
          if (widget.onOpenBot != null) {
            Navigator.of(context).pop();
            widget.onOpenBot!(bot);
          } else {
            _pushScreen(BotSettingsScreen(
              app: widget.app,
              botId: bot.id,
              device: widget.device,
            ));
          }
        },
      ));
    }

    for (final c in widget.app.conversations.where((c) => c.acceptsTurns)) {
      final title = smartConversationTitle(c);
      entries.add(_JumpEntry(
        id: 'chat-${c.id}',
        title: title,
        subtitle: c.isRoom ? 'Channel' : 'Conversation',
        searchText: '$title ${c.id}',
        icon: c.isRoom ? Icons.tag_rounded : Icons.chat_outlined,
        onSelect: () {
          if (widget.onOpenConversation != null) {
            Navigator.of(context).pop();
            widget.onOpenConversation!(c);
          } else {
            _pushScreen(ChatScreen(
              app: widget.app,
              conversation: c,
              device: widget.device,
            ));
          }
        },
      ));
    }

    return entries;
  }

  Widget _highlightedTitle(BuildContext context, String title, String query) {
    final ranges = fuzzyMatchRanges(query, title);
    if (ranges.isEmpty) {
      return Text(title, maxLines: 1, overflow: TextOverflow.ellipsis);
    }
    final scheme = Theme.of(context).colorScheme;
    final spans = <TextSpan>[];
    var cursor = 0;
    for (final r in ranges) {
      if (r.start > cursor) {
        spans.add(TextSpan(text: title.substring(cursor, r.start)));
      }
      spans.add(TextSpan(
        text: title.substring(r.start, r.end),
        style: TextStyle(
          color: scheme.primary,
          fontWeight: FontWeight.w700,
        ),
      ));
      cursor = r.end;
    }
    if (cursor < title.length) {
      spans.add(TextSpan(text: title.substring(cursor)));
    }
    return Text.rich(
      TextSpan(children: spans),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
    );
  }

  @override
  Widget build(BuildContext context) {
    final q = _query.text;
    final all = _buildEntries();
    final scored = <({int score, _JumpEntry entry})>[];
    for (final e in all) {
      final titleScore = fuzzyMatchScore(q, e.title);
      final fullScore = fuzzyMatchScore(q, e.searchText);
      final best = titleScore != null && fullScore != null
          ? (titleScore * 2 > fullScore ? titleScore * 2 : fullScore)
          : (titleScore != null ? titleScore * 2 : fullScore);
      if (best != null) {
        scored.add((score: best, entry: e));
      }
    }
    if (q.trim().isNotEmpty) {
      scored.sort((a, b) => b.score.compareTo(a.score));
    }

    final maxHeight = MediaQuery.sizeOf(context).height * 0.72;
    return SizedBox(
      height: maxHeight,
      child: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: TextField(
              key: const Key('quick-jump-field'),
              controller: _query,
              autofocus: true,
              decoration: InputDecoration(
                hintText: 'Quick Jump — tab, tool, bot, or chat',
                prefixIcon: const Icon(Icons.bolt_rounded),
                suffixIcon: q.isNotEmpty
                    ? IconButton(
                        tooltip: 'Clear',
                        icon: const Icon(Icons.close),
                        onPressed: () => setState(() => _query.clear()),
                      )
                    : null,
                border: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(14),
                ),
                isDense: true,
              ),
              onChanged: (_) => setState(() {}),
            ),
          ),
          Expanded(
            child: scored.isEmpty
                ? Center(
                    child: Text(
                      'No matching destinations.',
                      style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                            color:
                                Theme.of(context).colorScheme.onSurfaceVariant,
                          ),
                    ),
                  )
                : ListView.builder(
                    padding: const EdgeInsets.only(bottom: 16),
                    itemCount: scored.length,
                    itemBuilder: (context, i) {
                      final entry = scored[i].entry;
                      return ListTile(
                        key: Key('quick-jump-${entry.id}'),
                        leading: Icon(entry.icon),
                        title: _highlightedTitle(context, entry.title, q),
                        subtitle: Text(
                          entry.subtitle,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                        onTap: entry.onSelect,
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }
}
