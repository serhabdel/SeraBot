import 'package:material_ui/material_ui.dart';

import '../api_client.dart';
import '../app_state.dart';
import '../jobs_models.dart';
import '../widgets.dart';

/// Job search for the jobfinder bot (0.70): the profile, the watched sources
/// (read-only), tracked applications grouped by status, and a "Check now"
/// that lists new matches. Setting the profile up is done in chat, not here.
class JobsScreen extends StatefulWidget {
  const JobsScreen({super.key, required this.app});

  final AppState app;

  @override
  State<JobsScreen> createState() => _JobsScreenState();
}

class _JobsData {
  const _JobsData({
    required this.profile,
    required this.sources,
    required this.applications,
  });

  final JobProfile profile;
  final List<JobSource> sources;
  final List<JobApplication> applications;
}

class _JobsScreenState extends State<JobsScreen> {
  _JobsData? _data;
  String? _error;

  /// True when the server predates job search (404 on the jobs routes).
  bool _unsupported = false;
  bool _loading = true;
  bool _checking = false;
  List<JobMatch>? _matches;

  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _error = null;
      _unsupported = false;
    });
    final client = widget.app.client;
    try {
      final profile = await client.jobsProfile();
      final sources = await client.jobsSources();
      final applications = await client.jobsApplications();
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _data = _JobsData(
          profile: profile,
          sources: sources,
          applications: applications,
        );
        _loading = false;
      });
    } on ApiException catch (e) {
      if (!mounted || generation != _generation) {
        return;
      }
      setState(() {
        _unsupported = e.kind == ApiErrorKind.http && e.statusCode == 404;
        _error = _unsupported ? null : e.messageForUser();
        _loading = false;
      });
    }
  }

  Future<void> _check() async {
    setState(() => _checking = true);
    try {
      final result = await widget.app.client.jobsWatch();
      if (!mounted) {
        return;
      }
      setState(() {
        _matches = result.matches;
        _checking = false;
      });
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _checking = false);
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(e.messageForUser())));
    }
  }

  @override
  Widget build(BuildContext context) {
    final data = _data;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Job search'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            onPressed: _loading ? null : _load,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            if (_unsupported)
              const _UpdateNote()
            else if (_error != null) ...[
              ErrorBanner(_error!, onRetry: _load),
              const SizedBox(height: 12),
            ],
            if (data == null && !_unsupported && _error == null)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 48),
                child: Center(child: CircularProgressIndicator()),
              )
            else if (data != null) ...[
              _profileCard(context, data.profile),
              const SizedBox(height: 12),
              _checkCard(context),
              const SizedBox(height: 12),
              _sourcesCard(context, data.sources),
              const SizedBox(height: 12),
              _applicationsCard(context, data.applications),
            ],
          ],
        ),
      ),
    );
  }

  Widget _profileCard(BuildContext context, JobProfile profile) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('jobs-profile'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Profile', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            if (profile.isEmpty)
              Text(
                'No job profile yet. Ask jobfinder in chat to set one up.',
                style: theme.textTheme.bodySmall,
              )
            else ...[
              _line('Roles', profile.roles),
              _line('Cities', profile.cities),
              _line('Sectors', profile.sectors),
              _line('Include', profile.keywordsInclude),
              _line('Leave out', profile.keywordsExclude),
            ],
            if (profile.frequency.isNotEmpty)
              _text('Checks', profile.frequency),
            if (profile.trackerXlsx != null)
              _text('Tracker file', profile.trackerXlsx!),
            if (profile.materialsFolder != null)
              _text('Documents folder', profile.materialsFolder!),
          ],
        ),
      ),
    );
  }

  Widget _checkCard(BuildContext context) {
    final matches = _matches;
    return Card(
      key: const Key('jobs-check'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    'New matches',
                    style: Theme.of(context).textTheme.titleSmall,
                  ),
                ),
                FilledButton.tonal(
                  key: const Key('jobs-check-now'),
                  onPressed: _checking ? null : _check,
                  child: _checking
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Text('Check now'),
                ),
              ],
            ),
            if (matches == null)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(
                  'Check now to look for jobs that match your profile.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              )
            else if (matches.isEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(
                  'No new matches right now.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              )
            else
              for (final m in matches)
                ListTile(
                  dense: true,
                  contentPadding: EdgeInsets.zero,
                  title: Text(
                    m.title.isEmpty ? 'Untitled role' : m.title,
                    overflow: TextOverflow.ellipsis,
                  ),
                  subtitle: Text(
                    [
                      if (m.employer.isNotEmpty) m.employer,
                      if (m.city.isNotEmpty) m.city,
                      if (m.deadline.isNotEmpty) 'Closes ${m.deadline}',
                    ].join(' · '),
                  ),
                ),
          ],
        ),
      ),
    );
  }

  Widget _sourcesCard(BuildContext context, List<JobSource> sources) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('jobs-sources'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Sources', style: theme.textTheme.titleSmall),
            const SizedBox(height: 4),
            Text(
              'Set up in chat or on the desktop. Read-only here.',
              style: theme.textTheme.bodySmall,
            ),
            if (sources.isEmpty)
              const Padding(
                padding: EdgeInsets.only(top: 8),
                child: Text('No sources yet.'),
              )
            else
              for (final s in sources)
                ListTile(
                  dense: true,
                  contentPadding: EdgeInsets.zero,
                  title: Text(
                    s.label.isEmpty ? s.id : s.label,
                    overflow: TextOverflow.ellipsis,
                  ),
                  subtitle: s.url == null
                      ? null
                      : Text(s.url!, overflow: TextOverflow.ellipsis),
                  trailing: Text(s.autoWatch ? 'Auto-watch' : 'Manual'),
                ),
          ],
        ),
      ),
    );
  }

  Widget _applicationsCard(
    BuildContext context,
    List<JobApplication> applications,
  ) {
    final theme = Theme.of(context);
    final groups = groupByStatus(applications);
    return Card(
      key: const Key('jobs-applications'),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Applications', style: theme.textTheme.titleSmall),
            if (applications.isEmpty)
              const Padding(
                padding: EdgeInsets.only(top: 8),
                child: Text('No applications tracked yet.'),
              )
            else
              for (final entry in groups.entries) ...[
                Padding(
                  padding: const EdgeInsets.only(top: 12, bottom: 4),
                  child: Text(
                    '${_statusLabel(entry.key)} (${entry.value.length})',
                    style: theme.textTheme.labelLarge,
                  ),
                ),
                for (final a in entry.value)
                  ListTile(
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    title: Text(
                      a.title.isEmpty ? 'Untitled role' : a.title,
                      overflow: TextOverflow.ellipsis,
                    ),
                    subtitle: Text(
                      [
                        if (a.employer.isNotEmpty) a.employer,
                        if (a.updatedAt.isNotEmpty)
                          'Updated ${_day(a.updatedAt)}',
                      ].join(' · '),
                    ),
                  ),
              ],
          ],
        ),
      ),
    );
  }

  Widget _line(String label, List<String> values) {
    if (values.isEmpty) {
      return const SizedBox.shrink();
    }
    return _text(label, values.join(', '));
  }

  Widget _text(String label, String value) => Padding(
    padding: const EdgeInsets.only(bottom: 4),
    child: Text('$label: $value'),
  );

  String _statusLabel(String status) {
    if (status.isEmpty) {
      return 'No status';
    }
    return status[0].toUpperCase() + status.substring(1).replaceAll('_', ' ');
  }

  /// "2026-11-20T09:05:00Z" becomes "2026-11-20".
  String _day(String at) => at.length >= 10 ? at.substring(0, 10) : at;
}

class _UpdateNote extends StatelessWidget {
  const _UpdateNote();

  @override
  Widget build(BuildContext context) {
    return Card(
      key: const Key('jobs-update-note'),
      child: const Padding(
        padding: EdgeInsets.all(16),
        child: Text(
          'Job search needs SeraBot 0.70 on your server. Ask the owner to '
          'update it, then refresh.',
        ),
      ),
    );
  }
}
