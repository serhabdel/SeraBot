import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../models.dart';
import '../widgets.dart';
import 'approvals_screen.dart';

/// Personal tasks and calendar agenda (`/v1/tasks`, `/v1/tasks/sync/run`,
/// `/v1/tasks/calendar`).
class TasksScreen extends StatefulWidget {
  const TasksScreen({super.key, required this.app});

  final AppState app;

  @override
  State<TasksScreen> createState() => _TasksScreenState();
}

enum _TasksView { list, board, calendar }

class _TasksScreenState extends State<TasksScreen> {
  _TasksView _view = _TasksView.list;
  String _statusFilter = 'all';
  bool _loading = true;
  bool _syncing = false;
  String? _error;
  TasksResponse _data = const TasksResponse();
  List<CalendarEventItem> _events = const [];
  List<CalendarProviderWarning> _calendarWarnings = const [];

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final resp = await widget.app.client.tasks();
      List<CalendarEventItem> events = const [];
      List<CalendarProviderWarning> warnings = const [];
      if (resp.supported) {
        try {
          final cal = await widget.app.client.calendarEventsWithWarnings();
          events = cal.events;
          warnings = cal.warnings;
        } on ApiException {
          // Calendar endpoint is optional; fall back to due-dated tasks.
        }
      }
      if (!mounted) return;
      setState(() {
        _data = resp;
        _events = events;
        _calendarWarnings = warnings;
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e.messageForUser();
        _loading = false;
      });
    }
  }

  Future<void> _sync() async {
    setState(() => _syncing = true);
    final messenger = ScaffoldMessenger.of(context);
    try {
      await widget.app.client.syncTasksRun();
      await _load();
      if (mounted) {
        messenger.showSnackBar(const SnackBar(content: Text('Tasks synced')));
      }
    } on ApiException catch (e) {
      if (mounted) {
        messenger.showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    } finally {
      if (mounted) setState(() => _syncing = false);
    }
  }

  Future<void> _toggleDone(PersonalTask t) async {
    final nextStatus = t.isDone ? 'todo' : 'done';
    try {
      await widget.app.client.updateTask(t.id, status: nextStatus);
      await _load();
    } on ApiException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    }
  }

  Future<void> _deleteTask(PersonalTask t) async {
    final ok = await confirmAction(
      context,
      title: 'Delete task?',
      message: '"${t.title}" will be permanently removed.',
      confirmLabel: 'Delete',
    );
    if (!ok || !mounted) return;
    try {
      await widget.app.client.deleteTask(t.id);
      await _load();
    } on ApiException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    }
  }

  Future<void> _newTask() async {
    final titleCtrl = TextEditingController();
    final notesCtrl = TextEditingController();
    final dueCtrl = TextEditingController();
    final created = await showModalBottomSheet<bool>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheet) => Padding(
        padding: EdgeInsets.fromLTRB(
          16,
          0,
          16,
          MediaQuery.of(sheet).viewInsets.bottom + 16,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('New task', style: Theme.of(sheet).textTheme.titleLarge),
            const SizedBox(height: 12),
            TextField(
              key: const Key('task-title-field'),
              controller: titleCtrl,
              autofocus: true,
              maxLength: 200,
              decoration: const InputDecoration(
                labelText: 'Title',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            TextField(
              controller: dueCtrl,
              decoration: const InputDecoration(
                labelText: 'Due date (YYYY-MM-DD, optional)',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            TextField(
              controller: notesCtrl,
              maxLines: 2,
              decoration: const InputDecoration(
                labelText: 'Notes (optional)',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 12),
            FilledButton(
              onPressed: () => Navigator.of(sheet).pop(true),
              child: const Text('Add task'),
            ),
          ],
        ),
      ),
    );
    final title = titleCtrl.text.trim();
    final notes = notesCtrl.text.trim();
    final due = dueCtrl.text.trim();
    titleCtrl.dispose();
    notesCtrl.dispose();
    dueCtrl.dispose();
    if (created != true || title.isEmpty || !mounted) return;
    try {
      await widget.app.client.createTask(
        title: title,
        notes: notes.isEmpty ? null : notes,
        due: due.isEmpty ? null : due,
      );
      await _load();
    } on ApiException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(e.messageForUser())));
      }
    }
  }

  List<PersonalTask> get _filteredTasks {
    if (_statusFilter == 'all') return _data.tasks;
    return _data.tasks.where((t) => t.status == _statusFilter).toList();
  }

  /// Stat-tile counts (desktop Tasks polish parity): open, due soon,
  /// overdue, completed and upcoming calendar events. Due dates are
  /// `YYYY-MM-DD`; unparseable ones are ignored, never crash the screen.
  DateTime? _dueDate(PersonalTask t) {
    final raw = t.due;
    if (raw == null || raw.isEmpty) return null;
    final parsed = DateTime.tryParse(raw);
    if (parsed == null) return null;
    // Compare calendar days, not instants: a `YYYY-MM-DD` due parses as
    // UTC midnight, which is the previous evening in western timezones.
    return DateTime(parsed.year, parsed.month, parsed.day);
  }

  int get _openCount => _data.counts.open;

  int get _overdueCount {
    final today = DateTime.now();
    final day = DateTime(today.year, today.month, today.day);
    return _data.tasks.where((t) {
      final due = _dueDate(t);
      return !t.isDone && due != null && due.isBefore(day);
    }).length;
  }

  int get _dueSoonCount {
    final today = DateTime.now();
    final day = DateTime(today.year, today.month, today.day);
    final soon = day.add(const Duration(days: 2));
    return _data.tasks.where((t) {
      final due = _dueDate(t);
      return !t.isDone &&
          due != null &&
          !due.isBefore(day) &&
          !due.isAfter(soon);
    }).length;
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Tasks'),
        actions: [
          ListenableBuilder(
            listenable: widget.app,
            builder: (context, _) {
              final pending = widget.app.pendingApprovals;
              return IconButton(
                tooltip: pending == 0
                    ? 'Approvals'
                    : 'Approvals, $pending waiting',
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => ApprovalsScreen(app: widget.app),
                  ),
                ),
                icon: Badge(
                  isLabelVisible: pending > 0,
                  label: Text(pending > 99 ? '99+' : '$pending'),
                  child: const Icon(Icons.how_to_reg_outlined),
                ),
              );
            },
          ),
          if (_data.sync.enabled)
            IconButton(
              tooltip: 'Sync tasks (${_data.sync.provider})',
              onPressed: _syncing ? null : _sync,
              icon: _syncing
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.sync_rounded),
            ),
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      floatingActionButton: _data.supported
          ? FloatingActionButton.extended(
              heroTag: null,
              onPressed: _newTask,
              icon: const Icon(Icons.add_task_rounded),
              label: const Text('New task'),
            )
          : null,
      body: RefreshIndicator(
        onRefresh: _load,
        child: _loading && _data.tasks.isEmpty && _error == null
            ? ListView(
                physics: const AlwaysScrollableScrollPhysics(),
                children: const [
                  SizedBox(height: 120),
                  Center(child: CircularProgressIndicator()),
                ],
              )
            : ListView(
                physics: const AlwaysScrollableScrollPhysics(),
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 96),
                children: [
                  if (_error != null) ...[
                    ErrorBanner(_error!, onRetry: _load),
                    const SizedBox(height: 12),
                  ],
                  if (!_data.supported)
                    Card(
                      child: Padding(
                        padding: const EdgeInsets.all(16),
                        child: Text(
                          'This server does not expose /v1/tasks yet.',
                          style: Theme.of(context).textTheme.bodyMedium,
                        ),
                      ),
                    )
                  else ...[
                    Row(
                      children: [
                        Expanded(
                          child: SegmentedButton<_TasksView>(
                            segments: const [
                              ButtonSegment(
                                value: _TasksView.list,
                                icon: Icon(Icons.checklist_rounded, size: 18),
                                label: Text('Tasks'),
                              ),
                              ButtonSegment(
                                value: _TasksView.board,
                                icon: Icon(Icons.view_kanban_outlined, size: 18),
                                label: Text('Board'),
                              ),
                              ButtonSegment(
                                value: _TasksView.calendar,
                                icon: Icon(Icons.calendar_today_rounded, size: 16),
                                label: Text('Calendar'),
                              ),
                            ],
                            selected: {_view},
                            onSelectionChanged: (s) =>
                                setState(() => _view = s.first),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 12),
                    _statTiles(context),
                    const SizedBox(height: 12),
                    if (_data.sync.enabled) ...[
                      const SizedBox(height: 10),
                      Container(
                        padding: const EdgeInsets.symmetric(
                            horizontal: 12, vertical: 8),
                        decoration: BoxDecoration(
                          color: scheme.surfaceContainerHighest
                              .withValues(alpha: 0.6),
                          borderRadius: BorderRadius.circular(12),
                        ),
                        child: Row(
                          children: [
                            Icon(
                              _data.sync.lastOk == false
                                  ? Icons.sync_problem_rounded
                                  : Icons.cloud_done_outlined,
                              size: 16,
                              color: _data.sync.lastOk == false
                                  ? scheme.error
                                  : scheme.primary,
                            ),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                'Synced with ${_data.sync.provider}'
                                '${_data.sync.lastRun != null ? ' · ${shortTime(_data.sync.lastRun!)}' : ''}',
                                style: Theme.of(context).textTheme.bodySmall,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
                    const SizedBox(height: 12),
                    if (_view == _TasksView.list) ...[
                      SingleChildScrollView(
                        scrollDirection: Axis.horizontal,
                        child: Row(
                          children: [
                            _filterChip('all', 'All (${_data.tasks.length})'),
                            const SizedBox(width: 8),
                            _filterChip('todo', 'To do (${_data.counts.todo})'),
                            const SizedBox(width: 8),
                            _filterChip('in_progress',
                                'In progress (${_data.counts.inProgress})'),
                            const SizedBox(width: 8),
                            _filterChip('done', 'Done (${_data.counts.done})'),
                          ],
                        ),
                      ),
                      const SizedBox(height: 12),
                      if (_filteredTasks.isEmpty)
                        _emptyState(
                          context,
                          icon: Icons.check_circle_outline,
                          title: _statusFilter == 'all'
                              ? 'No tasks yet'
                              : 'Nothing ${_statusFilter == 'done' ? 'completed' : 'here'}',
                          subtitle: _statusFilter == 'all'
                              ? 'Tap New task to add your first one.'
                              : 'Tasks with this status will show up here.',
                        )
                      else
                        for (final t in _filteredTasks) _taskCard(context, t),
                    ] else if (_view == _TasksView.board) ...[
                      _kanbanBoard(context),
                    ] else ...[
                      _calendarAgenda(context),
                    ],
                  ],
                ],
              ),
      ),
    );
  }

  Widget _filterChip(String value, String label) {
    return FilterChip(
      selected: _statusFilter == value,
      label: Text(label),
      onSelected: (_) => setState(() => _statusFilter = value),
    );
  }

  /// Stat tiles (desktop Tasks polish parity): open, due soon, overdue,
  /// completed and upcoming calendar events, as a horizontal strip.
  Widget _statTiles(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    Widget tile(String label, int value, {bool alert = false}) {
      return Card(
        margin: const EdgeInsets.only(right: 8),
        elevation: 0,
        color: alert
            ? scheme.errorContainer.withValues(alpha: 0.5)
            : scheme.surfaceContainerLow,
        child: Container(
          width: 104,
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '$value',
                style: Theme.of(context).textTheme.headlineSmall?.copyWith(
                      color: alert ? scheme.onErrorContainer : null,
                    ),
              ),
              const SizedBox(height: 2),
              Text(
                label,
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                      color: alert
                          ? scheme.onErrorContainer
                          : scheme.onSurfaceVariant,
                    ),
              ),
            ],
          ),
        ),
      );
    }

    return SingleChildScrollView(
      key: const Key('tasks-stat-tiles'),
      scrollDirection: Axis.horizontal,
      child: Row(
        children: [
          tile('Open', _openCount),
          tile('Due soon', _dueSoonCount),
          tile('Overdue', _overdueCount, alert: _overdueCount > 0),
          tile('Done', _data.counts.done),
          tile('Events', _events.length),
        ],
      ),
    );
  }

  /// Kanban board (desktop parity): one column per status. Cards reuse the
  /// list card, so checking one off works the same here. Drag-and-drop
  /// between columns is deliberately not in this batch: status changes go
  /// through the checkbox (done) today.
  Widget _kanbanBoard(BuildContext context) {
    List<PersonalTask> inStatus(String status) =>
        _data.tasks.where((t) => t.status == status).toList();
    return SingleChildScrollView(
      key: const Key('tasks-board'),
      scrollDirection: Axis.horizontal,
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _boardColumn(context, 'To do', inStatus('todo')),
          _boardColumn(context, 'In progress', inStatus('in_progress')),
          _boardColumn(context, 'Done', inStatus('done')),
        ],
      ),
    );
  }

  Widget _boardColumn(
    BuildContext context,
    String title,
    List<PersonalTask> tasks,
  ) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: 264,
      margin: const EdgeInsets.only(right: 12),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(14),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(4, 2, 4, 8),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    title,
                    style: Theme.of(context).textTheme.titleSmall,
                  ),
                ),
                Text(
                  '${tasks.length}',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ],
            ),
          ),
          if (tasks.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 24),
              child: Center(
                child: Text(
                  'No tasks',
                  style: Theme.of(context).textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                ),
              ),
            )
          else
            for (final t in tasks) _taskCard(context, t),
        ],
      ),
    );
  }

  /// Shared empty state: icon, title and a next step, never a bare line.
  Widget _emptyState(
    BuildContext context, {
    required IconData icon,
    required String title,
    required String subtitle,
  }) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      key: const Key('tasks-empty'),
      padding: const EdgeInsets.only(top: 64),
      child: Center(
        child: Column(
          children: [
            Icon(icon, size: 48, color: scheme.onSurfaceVariant),
            const SizedBox(height: 12),
            Text(title, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 4),
            Text(
              subtitle,
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
              textAlign: TextAlign.center,
            ),
          ],
        ),
      ),
    );
  }

  Widget _taskCard(BuildContext context, PersonalTask t) {
    final scheme = Theme.of(context).colorScheme;
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      child: ListTile(
        leading: Checkbox(
          value: t.isDone,
          onChanged: (_) => _toggleDone(t),
        ),
        title: Text(
          t.title,
          style: TextStyle(
            decoration: t.isDone ? TextDecoration.lineThrough : null,
            color: t.isDone ? scheme.onSurfaceVariant : null,
          ),
        ),
        subtitle: (t.notes.isNotEmpty || (t.due != null && t.due!.isNotEmpty))
            ? Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (t.notes.isNotEmpty)
                    Text(
                      t.notes,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                  if (t.due != null && t.due!.isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: Row(
                        children: [
                          Icon(Icons.event_outlined,
                              size: 14, color: scheme.primary),
                          const SizedBox(width: 4),
                          Text(
                            t.due!,
                            style: Theme.of(context)
                                .textTheme
                                .labelSmall
                                ?.copyWith(color: scheme.primary),
                          ),
                        ],
                      ),
                    ),
                ],
              )
            : null,
        trailing: IconButton(
          tooltip: 'Delete task',
          icon: const Icon(Icons.delete_outline, size: 20),
          onPressed: () => _deleteTask(t),
        ),
      ),
    );
  }

  Widget _calendarAgenda(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final isDark = scheme.brightness == Brightness.dark;
    final warnBg = isDark ? const Color(0xFF3E2A00) : const Color(0xFFFFF8E1);
    final warnBorder =
        isDark ? const Color(0xFFFFB300) : const Color(0xFFF59E0B);
    final warnFg = isDark ? const Color(0xFFFFE082) : const Color(0xFF78350F);
    final dueTasks =
        _data.tasks.where((t) => t.due != null && t.due!.isNotEmpty).toList();
    if (_events.isEmpty && dueTasks.isEmpty && _calendarWarnings.isEmpty) {
      return _emptyState(
        context,
        icon: Icons.event_available_outlined,
        title: 'Nothing coming up',
        subtitle: 'No calendar events or due tasks in this view.',
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final w in _calendarWarnings)
          Card(
            key: Key('calendar-warning-${w.provider}'),
            margin: const EdgeInsets.only(bottom: 10),
            elevation: 0,
            color: warnBg,
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(12),
              side: BorderSide(color: warnBorder.withValues(alpha: 0.5)),
            ),
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(Icons.warning_amber_rounded, color: warnBorder, size: 20),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          w.message,
                          style: Theme.of(context)
                              .textTheme
                              .bodyMedium
                              ?.copyWith(
                                color: warnFg,
                                fontWeight: FontWeight.w600,
                              ),
                        ),
                        if (w.hint != null && w.hint!.isNotEmpty) ...[
                          const SizedBox(height: 4),
                          Text(
                            w.hint!,
                            style: Theme.of(context)
                                .textTheme
                                .bodySmall
                                ?.copyWith(color: warnFg),
                          ),
                        ],
                      ],
                    ),
                  ),
                ],
              ),
            ),
          ),
        if (_events.isEmpty && dueTasks.isEmpty)
          _emptyState(
            context,
            icon: Icons.event_available_outlined,
            title: 'Nothing coming up',
            subtitle: 'No calendar events or due tasks in this view.',
          ),
        for (final e in _events)
          Card(
            margin: const EdgeInsets.only(bottom: 8),
            child: ListTile(
              leading: CircleAvatar(
                backgroundColor: scheme.primaryContainer,
                child: Icon(Icons.event_rounded,
                    size: 18, color: scheme.onPrimaryContainer),
              ),
              title: Text(e.title),
              subtitle: Text(
                '${e.start}${e.provider != null ? ' · ${e.provider}' : ''}',
              ),
            ),
          ),
        for (final t in dueTasks)
          Card(
            margin: const EdgeInsets.only(bottom: 8),
            child: ListTile(
              leading: Checkbox(
                value: t.isDone,
                onChanged: (_) => _toggleDone(t),
              ),
              title: Text(t.title),
              subtitle: Text('Due ${t.due}'),
            ),
          ),
      ],
    );
  }
}
