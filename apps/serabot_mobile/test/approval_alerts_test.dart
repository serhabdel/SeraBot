import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/app_state.dart';

final _token = 'q' * 43;

Map<String, dynamic> _approval(String id) => {
  'id': id,
  'action_id': 'act_$id',
  'tool': 'synthetic.tool',
  'summary': 'Synthetic approval',
  'expires_at': '2026-11-21T00:00:00Z',
};

void main() {
  test('first load is silent; each new approval alerts once', () async {
    var waiting = <Map<String, dynamic>>[];
    final client = ApiClient.connect(
      'https://example.com',
      _token,
      getRetryDelays: const [],
      httpClient: MockClient((request) async {
        expect(request.url.path, '/v1/approvals');
        return http.Response(
          jsonEncode({'approvals': waiting}),
          200,
          headers: {'content-type': 'application/json'},
        );
      }),
    );
    final app = AppState(client);
    final alerts = <int>[];
    final sub = app.approvalAlerts.listen(alerts.add);

    Future<void> refresh() async {
      await app.refreshApprovals();
      // Stream events arrive on a later microtask.
      await Future<void>.delayed(Duration.zero);
    }

    waiting = [_approval('appr_one')];
    await refresh();
    expect(alerts, isEmpty, reason: 'what is already waiting does not alert');
    expect(app.pendingApprovals, 1);

    waiting = [_approval('appr_one'), _approval('appr_two')];
    await refresh();
    await refresh();
    expect(alerts, [1], reason: 'the same set again does not alert twice');
    expect(app.pendingApprovals, 2);

    waiting = [_approval('appr_three'), _approval('appr_four')];
    await refresh();
    expect(alerts, [1, 2]);

    await sub.cancel();
    app.dispose();
    client.close();
  });
}
