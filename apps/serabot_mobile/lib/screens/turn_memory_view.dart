import 'package:material_ui/material_ui.dart';

import '../turn_memory.dart';

/// "Memories used" and "Memories saved" for one reply (`turn.memory`, 0.61).
/// Shown inside the reply's activity panel, under its steps. Text the server
/// withheld is said so instead of left blank.
class TurnMemoryView extends StatelessWidget {
  const TurnMemoryView({super.key, required this.memory});

  final TurnMemory memory;

  @override
  Widget build(BuildContext context) {
    return Column(
      key: const Key('turn-memory'),
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (memory.recalledTotal > 0 || memory.recalled.isNotEmpty)
          _Side(
            title: 'Memories used',
            icon: Icons.psychology_outlined,
            items: memory.recalled,
            total: memory.recalledTotal,
          ),
        if (memory.savedTotal > 0 || memory.saved.isNotEmpty)
          _Side(
            title: 'Memories saved',
            icon: Icons.bookmark_added_outlined,
            items: memory.saved,
            total: memory.savedTotal,
          ),
      ],
    );
  }
}

class _Side extends StatelessWidget {
  const _Side({
    required this.title,
    required this.icon,
    required this.items,
    required this.total,
  });

  final String title;
  final IconData icon;
  final List<TurnMemoryItem> items;
  final int total;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final count = total > items.length ? total : items.length;
    final hidden = count - items.length;
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(icon, size: 18),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  '$title ($count)',
                  style: theme.textTheme.titleSmall,
                ),
              ),
            ],
          ),
          for (final item in items) _MemoryRow(item: item),
          if (hidden > 0)
            Padding(
              padding: const EdgeInsets.only(left: 24, top: 4),
              child: Text('and $hidden more', style: theme.textTheme.bodySmall),
            ),
        ],
      ),
    );
  }
}

class _MemoryRow extends StatelessWidget {
  const _MemoryRow({required this.item});

  final TurnMemoryItem item;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    final text = item.excerpt.isEmpty
        ? 'Text withheld by the server'
        : item.excerpt;
    final details = <String>[
      item.originLabel,
      if (item.pending) 'Held for your review',
      if (item.replaces.isNotEmpty) 'Replaces: ${item.replaces.join('; ')}',
    ];
    return Padding(
      padding: const EdgeInsets.only(left: 24, top: 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(text),
          Text(details.join(' · '), style: muted),
        ],
      ),
    );
  }
}
