import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:serabot_mobile/updater.dart';

const _base = 'https://github.com/serhabdel/SeraBot/releases/download';

AppVersion v(String s) => AppVersion.parse(s)!;

String releaseJson(String tag, {List<Map<String, Object>>? assets, bool pre = false}) =>
    jsonEncode({
      'tag_name': tag,
      'body': 'Notes\u0000 here',
      'prerelease': pre,
      'assets': assets ??
          [
            {
              'name': 'serabot-mobile-$tag.apk',
              'size': 10,
              'browser_download_url': '$_base/$tag/serabot-mobile-$tag.apk',
            },
            {
              'name': 'serabot-mobile-$tag.apk.sha256',
              'size': 90,
              'browser_download_url': '$_base/$tag/serabot-mobile-$tag.apk.sha256',
            },
          ],
    });

void main() {
  group('version compare', () {
    test('parses tags and ignores build metadata', () {
      expect(v('v0.4.0').toString(), '0.4.0');
      expect(v('0.4.0+10').toString(), '0.4.0');
      expect(AppVersion.parse('0.4'), isNull);
      expect(AppVersion.parse('v1.x.0'), isNull);
      expect(AppVersion.parse('01.0.0'), isNull);
      expect(AppVersion.parse(''), isNull);
    });

    test('orders numerically, not lexically', () {
      expect(v('0.10.0').compareTo(v('0.9.9')), greaterThan(0));
      expect(v('1.0.0').compareTo(v('0.99.99')), greaterThan(0));
      expect(v('0.4.0') == v('0.4.0'), isTrue);
    });

    test('release outranks its pre-release', () {
      expect(v('1.0.0').compareTo(v('1.0.0-rc.1')), greaterThan(0));
      expect(v('1.0.0-rc.2').compareTo(v('1.0.0-rc.10')), lessThan(0));
    });

    test('isNewer: stable installs ignore pre-releases', () {
      expect(isNewer(v('0.5.0'), v('0.4.0')), isTrue);
      expect(isNewer(v('0.4.0'), v('0.4.0')), isFalse);
      expect(isNewer(v('0.3.9'), v('0.4.0')), isFalse);
      expect(isNewer(v('0.5.0-rc.1'), v('0.4.0')), isFalse);
      expect(isNewer(v('0.5.0-rc.2'), v('0.5.0-rc.1')), isTrue);
      expect(isNewer(v('0.5.0'), v('0.5.0-rc.1')), isTrue);
    });
  });

  group('asset selection', () {
    test('picks exact-name apk and checksum', () {
      final r = ReleaseInfo.parse(releaseJson('v0.5.0'));
      final sel = selectAssets(r)!;
      expect(sel.apk.name, 'serabot-mobile-v0.5.0.apk');
      expect(sel.checksum.name, 'serabot-mobile-v0.5.0.apk.sha256');
      expect(r.notes, 'Notes here');
    });

    test('rejects missing checksum, wrong name, oversize, bad url', () {
      expect(
        selectAssets(ReleaseInfo.parse(releaseJson('v0.5.0', assets: [
          {
            'name': 'serabot-mobile-v0.5.0.apk',
            'size': 10,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-v0.5.0.apk',
          },
        ]))),
        isNull,
      );
      expect(
        selectAssets(ReleaseInfo.parse(releaseJson('v0.5.0', assets: [
          {
            'name': 'serabot-mobile-latest.apk',
            'size': 10,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-latest.apk',
          },
          {
            'name': 'serabot-mobile-latest.apk.sha256',
            'size': 90,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-latest.apk.sha256',
          },
        ]))),
        isNull,
      );
      expect(
        selectAssets(ReleaseInfo.parse(releaseJson('v0.5.0', assets: [
          {
            'name': 'serabot-mobile-v0.5.0.apk',
            'size': maxApkBytes + 1,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-v0.5.0.apk',
          },
          {
            'name': 'serabot-mobile-v0.5.0.apk.sha256',
            'size': 90,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-v0.5.0.apk.sha256',
          },
        ]))),
        isNull,
      );
      expect(
        selectAssets(ReleaseInfo.parse(releaseJson('v0.5.0', assets: [
          {
            'name': 'serabot-mobile-v0.5.0.apk',
            'size': 10,
            'browser_download_url': 'https://evil.example/serabot-mobile-v0.5.0.apk',
          },
          {
            'name': 'serabot-mobile-v0.5.0.apk.sha256',
            'size': 90,
            'browser_download_url': '$_base/v0.5.0/serabot-mobile-v0.5.0.apk.sha256',
          },
        ]))),
        isNull,
      );
    });

    test('malformed release json is rejected', () {
      expect(() => ReleaseInfo.parse('[]'), throwsA(isA<UpdateException>()));
      expect(() => ReleaseInfo.parse('nope'), throwsA(isA<UpdateException>()));
    });
  });

  group('url allowlist', () {
    test('accepts this repo release downloads and GitHub CDN hosts', () {
      expect(isAllowedAssetUrl('$_base/v1.0.0/a.apk'), isTrue);
      expect(
        isAllowedAssetUrl('https://objects.githubusercontent.com/x/y?z=1'),
        isTrue,
      );
    });

    test('rejects http, other hosts, other repos, tricks', () {
      expect(isAllowedAssetUrl('http://github.com/serhabdel/SeraBot/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://evil.example/serhabdel/SeraBot/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://github.com/other/repo/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://github.com.evil.example/serhabdel/SeraBot/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://user:pw@github.com/serhabdel/SeraBot/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://github.com:8443/serhabdel/SeraBot/releases/download/v1/a'), isFalse);
      expect(isAllowedAssetUrl('https://github.com/serhabdel/SeraBot/releases/download/../x'), isFalse);
      expect(isAllowedAssetUrl('https://github.com/serhabdel/SeraBot/releases/download/v1/a#f'), isFalse);
      expect(isAllowedAssetUrl('not a url'), isFalse);
    });
  });

  group('checksum', () {
    final hex = 'ab' * 32;

    test('parses sha256sum and bare formats', () {
      expect(parseChecksum('$hex  app.apk\n', 'app.apk'), hasLength(32));
      expect(parseChecksum('$hex *app.apk', 'app.apk'), hasLength(32));
      expect(parseChecksum(hex, 'app.apk'), hasLength(32));
    });

    test('rejects wrong name, short hash, garbage', () {
      expect(parseChecksum('$hex  other.apk', 'app.apk'), isNull);
      expect(parseChecksum('abcd  app.apk', 'app.apk'), isNull);
      expect(parseChecksum('zz' * 32, 'app.apk'), isNull);
      expect(parseChecksum('', 'app.apk'), isNull);
    });

    test('digestsEqual compares bytes', () {
      expect(digestsEqual([1, 2], [1, 2]), isTrue);
      expect(digestsEqual([1, 2], [1, 3]), isFalse);
      expect(digestsEqual([1], [1, 2]), isFalse);
    });
  });

  group('download verification', () {
    late Directory dir;
    setUp(() => dir = Directory.systemTemp.createTempSync('serabot_upd'));
    tearDown(() => dir.deleteSync(recursive: true));

    final apkBytes = utf8.encode('fake-apk-bytes');
    final apkHash = sha256.convert(apkBytes).toString();

    UpdateChecker checker(String hashLine, {String? redirectHost}) {
      final client = MockClient.streaming((request, _) async {
        final path = request.url.path;
        if (path.endsWith('.sha256')) {
          return http.StreamedResponse(Stream.value(utf8.encode(hashLine)), 200);
        }
        if (request.url.host == 'github.com') {
          return http.StreamedResponse(
            const Stream.empty(),
            302,
            headers: {'location': 'https://${redirectHost ?? 'objects.githubusercontent.com'}/blob'},
          );
        }
        return http.StreamedResponse(Stream.value(apkBytes), 200);
      });
      return UpdateChecker(current: v('0.4.0'), cacheDir: dir, client: client);
    }

    AvailableUpdate update() => AvailableUpdate(
          version: v('0.5.0'),
          tag: 'v0.5.0',
          notes: '',
          apk: const ReleaseAsset(
            name: 'serabot-mobile-v0.5.0.apk',
            size: 14,
            url: '$_base/v0.5.0/serabot-mobile-v0.5.0.apk',
          ),
          checksum: const ReleaseAsset(
            name: 'serabot-mobile-v0.5.0.apk.sha256',
            size: 90,
            url: '$_base/v0.5.0/serabot-mobile-v0.5.0.apk.sha256',
          ),
        );

    test('matching hash yields the file', () async {
      final c = checker('$apkHash  serabot-mobile-v0.5.0.apk');
      final f = await c.download(update());
      expect(await f.readAsBytes(), apkBytes);
    });

    test('mismatched hash discards the download', () async {
      final c = checker('${'0' * 64}  serabot-mobile-v0.5.0.apk');
      await expectLater(
        c.download(update()),
        throwsA(isA<UpdateException>()
            .having((e) => e.kind, 'kind', UpdateErrorKind.hashMismatch)),
      );
      expect(dir.listSync(), isEmpty);
    });

    test('redirect to a non-allowlisted host is refused', () async {
      final c = checker('$apkHash  serabot-mobile-v0.5.0.apk', redirectHost: 'evil.example');
      await expectLater(
        c.download(update()),
        throwsA(isA<UpdateException>()
            .having((e) => e.kind, 'kind', UpdateErrorKind.untrustedUrl)),
      );
    });

    test('check() reports newer release and skips equal/older', () async {
      UpdateChecker api(String tag) => UpdateChecker(
            current: v('0.4.0'),
            cacheDir: dir,
            client: MockClient((_) async => http.Response(releaseJson(tag), 200)),
          );
      expect((await api('v0.5.0').check())!.tag, 'v0.5.0');
      expect(await api('v0.4.0').check(), isNull);
      expect(await api('v0.3.0').check(), isNull);
    });
  });
}
