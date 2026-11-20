import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/jobs_models.dart';

/// Synthetic profile, as the jobfinder bot would store it.
Map<String, dynamic> _profileBody() => {
  'roles': ['Synthetic analyst', '', 'Synthetic lead'],
  'cities': ['Examplecity'],
  'sectors': <Object>[],
  'keywords_include': ['synthetic', 'data'],
  'keywords_exclude': ['unpaid'],
  'frequency': 'daily',
  'tracker_xlsx': 'jobs/tracker.xlsx',
  'materials_folder': null,
};

void main() {
  group('JobProfile.fromJson', () {
    test('reads the lists, dropping empty entries', () {
      final profile = JobProfile.fromJson(_profileBody());

      expect(profile.roles, ['Synthetic analyst', 'Synthetic lead']);
      expect(profile.cities, ['Examplecity']);
      expect(profile.sectors, isEmpty);
      expect(profile.keywordsInclude, ['synthetic', 'data']);
      expect(profile.keywordsExclude, ['unpaid']);
      expect(profile.frequency, 'daily');
      expect(profile.trackerXlsx, 'jobs/tracker.xlsx');
      expect(profile.materialsFolder, isNull);
      expect(profile.isEmpty, isFalse);
    });

    test('accepts the profile under a profile key', () {
      final profile = JobProfile.fromJson({'profile': _profileBody()});

      expect(profile.roles.first, 'Synthetic analyst');
      expect(profile.frequency, 'daily');
    });

    test('an empty profile reports itself as empty', () {
      final profile = JobProfile.fromJson(const {});

      expect(profile.isEmpty, isTrue);
      expect(profile.roles, isEmpty);
      expect(profile.frequency, '');
      expect(profile.trackerXlsx, isNull);
    });

    test('wrong types fall back to defaults', () {
      final profile = JobProfile.fromJson({
        'roles': 'Analyst',
        'cities': [
          1,
          null,
          {'x': 1},
        ],
        'frequency': 12,
        'tracker_xlsx': '   ',
      });

      expect(profile.roles, isEmpty, reason: 'a string is not a list');
      expect(profile.cities, isEmpty, reason: 'non-text entries are dropped');
      expect(profile.frequency, '');
      expect(profile.trackerXlsx, isNull, reason: 'blank text is absent');
      expect(profile.isEmpty, isTrue);
    });
  });

  group('job sources and applications', () {
    test('sources keep read-only fields and skip rows without an id', () {
      final sources = jobSourcesFrom({
        'sources': [
          {
            'id': 'src_1',
            'label': 'Synthetic board',
            'url': 'https://jobs.example.test/list',
            'auto_watch': true,
          },
          {'label': 'no id, skipped'},
          {'id': 'src_2', 'label': 'Manual one', 'auto_watch': 'yes'},
        ],
      });

      expect(sources.map((s) => s.id), ['src_1', 'src_2']);
      expect(sources.first.url, 'https://jobs.example.test/list');
      expect(sources.first.autoWatch, isTrue);
      expect(sources.last.url, isNull);
      expect(
        sources.last.autoWatch,
        isFalse,
        reason: 'only a true flag counts',
      );
    });

    test('a missing sources key is an empty list', () {
      expect(jobSourcesFrom(const {}), isEmpty);
    });

    test('applications group by status in first-seen order', () {
      final applications = jobApplicationsFrom({
        'applications': [
          {
            'id': 'app_1',
            'title': 'Synthetic analyst',
            'employer': 'Example Ltd',
            'status': 'Applied',
            'updated_at': '2026-11-19T10:00:00Z',
            'url': 'https://jobs.example.test/1',
          },
          {'id': 'app_2', 'title': 'Synthetic lead', 'status': 'interview'},
          {'id': 'app_3', 'title': 'Synthetic data', 'status': 'applied'},
          {'id': 'app_4', 'title': 'No status yet'},
          {'title': 'no id, skipped', 'status': 'applied'},
        ],
      });

      expect(applications, hasLength(4));
      expect(
        applications.first.status,
        'applied',
        reason: 'status is lowercased',
      );
      expect(applications.first.employer, 'Example Ltd');
      expect(applications.first.updatedAt, '2026-11-19T10:00:00Z');

      final groups = groupByStatus(applications);
      expect(groups.keys.toList(), ['applied', 'interview', '']);
      expect(groups['applied']!.map((a) => a.id), ['app_1', 'app_3']);
      expect(groups['']!.single.id, 'app_4');
    });
  });

  group('JobWatchResult.fromJson', () {
    test('reads matches with their deadline and link', () {
      final result = JobWatchResult.fromJson({
        'matches': [
          {
            'title': 'Synthetic analyst',
            'employer': 'Example Ltd',
            'city': 'Examplecity',
            'deadline': '2026-12-01',
            'url': 'https://jobs.example.test/42',
          },
          'junk',
        ],
      });

      expect(result.matches, hasLength(1));
      expect(result.matches.single.city, 'Examplecity');
      expect(result.matches.single.deadline, '2026-12-01');
      expect(result.matches.single.url, 'https://jobs.example.test/42');
    });

    test('no matches, or a bad body, is an empty result', () {
      expect(JobWatchResult.fromJson(const {}).matches, isEmpty);
      expect(JobWatchResult.fromJson({'matches': 'none'}).matches, isEmpty);
    });
  });
}
