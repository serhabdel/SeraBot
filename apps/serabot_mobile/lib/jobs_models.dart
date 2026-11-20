// Job search (`/v1/jobs/profile`, `/v1/jobs/sources`, `/v1/jobs/applications`,
// `POST /v1/jobs/watch`; the jobfinder bot, 0.70). Read-only on the phone.
// Every field is optional on the wire: missing or wrong-typed values fall
// back to empty defaults, and lists are bounded.

import 'usage_insights_models.dart'
    show insightOptText, insightRows, insightText;

const _maxListItems = 50;

/// Text values of a JSON list, empties dropped, bounded.
List<String> _texts(Object? v, [int max = 120]) {
  if (v is! List) {
    return const [];
  }
  return v
      .map((e) => insightText(e, max))
      .where((s) => s.isNotEmpty)
      .take(_maxListItems)
      .toList();
}

Map<String, dynamic> _map(Object? v) =>
    v is Map<String, dynamic> ? v : const <String, dynamic>{};

class JobProfile {
  const JobProfile({
    this.roles = const [],
    this.cities = const [],
    this.sectors = const [],
    this.keywordsInclude = const [],
    this.keywordsExclude = const [],
    this.frequency = '',
    this.trackerXlsx,
    this.materialsFolder,
  });

  final List<String> roles;
  final List<String> cities;
  final List<String> sectors;
  final List<String> keywordsInclude;
  final List<String> keywordsExclude;
  final String frequency;
  final String? trackerXlsx;
  final String? materialsFolder;

  /// True when the member has not set up anything yet.
  bool get isEmpty =>
      roles.isEmpty &&
      cities.isEmpty &&
      sectors.isEmpty &&
      keywordsInclude.isEmpty &&
      keywordsExclude.isEmpty;

  /// Accepts the profile as the body itself or under a `profile` key.
  factory JobProfile.fromJson(Map<String, dynamic> json) {
    final body = json['profile'] is Map<String, dynamic>
        ? _map(json['profile'])
        : json;
    return JobProfile(
      roles: _texts(body['roles']),
      cities: _texts(body['cities']),
      sectors: _texts(body['sectors']),
      keywordsInclude: _texts(body['keywords_include']),
      keywordsExclude: _texts(body['keywords_exclude']),
      frequency: insightText(body['frequency'], 40),
      trackerXlsx: insightOptText(body['tracker_xlsx'], 200),
      materialsFolder: insightOptText(body['materials_folder'], 200),
    );
  }
}

class JobSource {
  const JobSource({
    required this.id,
    required this.label,
    this.url,
    this.autoWatch = false,
  });

  final String id;
  final String label;
  final String? url;
  final bool autoWatch;

  factory JobSource.fromJson(Map<String, dynamic> json) => JobSource(
    id: insightText(json['id'], 80),
    label: insightText(json['label'], 120),
    url: insightOptText(json['url'], 300),
    autoWatch: json['auto_watch'] == true,
  );
}

class JobApplication {
  const JobApplication({
    required this.id,
    required this.title,
    this.employer = '',
    this.status = '',
    this.updatedAt = '',
    this.url,
  });

  final String id;
  final String title;
  final String employer;

  /// Free text from the tracker, grouped on screen. Empty means "no status".
  final String status;
  final String updatedAt;
  final String? url;

  factory JobApplication.fromJson(Map<String, dynamic> json) => JobApplication(
    id: insightText(json['id'], 80),
    title: insightText(json['title'], 160),
    employer: insightText(json['employer'], 120),
    status: insightText(json['status'], 60).toLowerCase(),
    updatedAt: insightText(json['updated_at'], 40),
    url: insightOptText(json['url'], 300),
  );
}

class JobMatch {
  const JobMatch({
    required this.title,
    this.employer = '',
    this.city = '',
    this.deadline = '',
    this.url,
  });

  final String title;
  final String employer;
  final String city;
  final String deadline;
  final String? url;

  factory JobMatch.fromJson(Map<String, dynamic> json) => JobMatch(
    title: insightText(json['title'], 160),
    employer: insightText(json['employer'], 120),
    city: insightText(json['city'], 80),
    deadline: insightText(json['deadline'], 40),
    url: insightOptText(json['url'], 300),
  );
}

/// The result of `POST /v1/jobs/watch`: the matches found by a check now.
class JobWatchResult {
  const JobWatchResult({this.matches = const []});

  final List<JobMatch> matches;

  factory JobWatchResult.fromJson(Map<String, dynamic> json) => JobWatchResult(
    matches: insightRows(json['matches']).map(JobMatch.fromJson).toList(),
  );
}

/// Sources and applications come as `{"sources": [...]}` and
/// `{"applications": [...]}`. Rows without an id or title are dropped.
List<JobSource> jobSourcesFrom(Map<String, dynamic> json) =>
    insightRows(json['sources'])
        .map(JobSource.fromJson)
        .where((s) => s.id.isNotEmpty)
        .toList();

List<JobApplication> jobApplicationsFrom(Map<String, dynamic> json) =>
    insightRows(json['applications'])
        .map(JobApplication.fromJson)
        .where((a) => a.id.isNotEmpty)
        .toList();

/// Applications grouped by status, in the order each status first appears.
Map<String, List<JobApplication>> groupByStatus(
  List<JobApplication> applications,
) {
  final groups = <String, List<JobApplication>>{};
  for (final application in applications) {
    groups.putIfAbsent(application.status, () => []).add(application);
  }
  return groups;
}
