import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:http/http.dart' as http;

/// In-app updater for sideloaded builds (before Play distribution).
///
/// Mirrors the desktop updater's rules: the release is discovered through the
/// GitHub API, assets are picked by exact name, URLs must be HTTPS GitHub
/// release downloads, sizes are capped, and the APK is only handed to the
/// installer after its SHA-256 matches the checksum asset in the same release.

const releasesRepo = 'serhabdel/SeraBot';
const maxReleaseJson = 512 * 1024;
const maxChecksumBytes = 4 * 1024;
const maxApkBytes = 200 * 1024 * 1024;
const maxNotesChars = 4000;
const _apiTimeout = Duration(seconds: 15);
const _downloadTimeout = Duration(minutes: 5);

/// Hosts a release asset may be fetched from (initial URL and every redirect).
/// GitHub serves release assets from objects.githubusercontent.com, and newer
/// releases from release-assets.githubusercontent.com.
const allowedAssetHosts = {
  'github.com',
  'objects.githubusercontent.com',
  'release-assets.githubusercontent.com',
};

enum UpdateErrorKind {
  network,
  http,
  invalidRelease,
  tooLarge,
  hashMismatch,
  storage,
  untrustedUrl,
}

class UpdateException implements Exception {
  const UpdateException(this.kind, [this.detail]);
  final UpdateErrorKind kind;
  final int? detail;

  @override
  String toString() => switch (kind) {
        UpdateErrorKind.network => 'Update check failed: the network is unavailable.',
        UpdateErrorKind.http when detail == 403 || detail == 429 =>
          'Update check was rate-limited by GitHub. Try again later.',
        UpdateErrorKind.http => 'Update check failed with HTTP $detail.',
        UpdateErrorKind.invalidRelease => 'The latest release metadata is malformed.',
        UpdateErrorKind.tooLarge => 'The update exceeds the download safety limit.',
        UpdateErrorKind.hashMismatch =>
          'SHA-256 verification failed. The update was discarded.',
        UpdateErrorKind.storage => 'The update could not be stored locally.',
        UpdateErrorKind.untrustedUrl => 'The update was served from an untrusted address.',
      };
}

/// Parsed semantic version (`1.2.3` or `1.2.3-rc.1`, optional leading `v`).
class AppVersion implements Comparable<AppVersion> {
  const AppVersion(this.major, this.minor, this.patch, [this.pre = const []]);
  final int major;
  final int minor;
  final int patch;
  final List<String> pre;

  bool get isPrerelease => pre.isNotEmpty;

  /// Null when [raw] is not a valid version. Build metadata (`+9`) is ignored.
  static AppVersion? parse(String raw) {
    var text = raw.trim();
    if (text.startsWith('v') || text.startsWith('V')) {
      text = text.substring(1);
    }
    final plus = text.indexOf('+');
    if (plus >= 0) {
      text = text.substring(0, plus);
    }
    final dash = text.indexOf('-');
    final core = dash >= 0 ? text.substring(0, dash) : text;
    final preText = dash >= 0 ? text.substring(dash + 1) : '';
    final parts = core.split('.');
    if (parts.length != 3) {
      return null;
    }
    final nums = <int>[];
    for (final part in parts) {
      if (!RegExp(r'^(0|[1-9][0-9]{0,8})$').hasMatch(part)) {
        return null;
      }
      nums.add(int.parse(part));
    }
    var pre = const <String>[];
    if (dash >= 0) {
      pre = preText.split('.');
      if (pre.any((id) => !RegExp(r'^[0-9A-Za-z-]+$').hasMatch(id))) {
        return null;
      }
    }
    return AppVersion(nums[0], nums[1], nums[2], pre);
  }

  @override
  int compareTo(AppVersion other) {
    for (final pair in [
      (major, other.major),
      (minor, other.minor),
      (patch, other.patch),
    ]) {
      if (pair.$1 != pair.$2) {
        return pair.$1.compareTo(pair.$2);
      }
    }
    if (pre.isEmpty && other.pre.isEmpty) {
      return 0;
    }
    if (pre.isEmpty) {
      return 1; // a release outranks its pre-releases
    }
    if (other.pre.isEmpty) {
      return -1;
    }
    for (var i = 0; i < pre.length && i < other.pre.length; i++) {
      final a = pre[i];
      final b = other.pre[i];
      final an = int.tryParse(a);
      final bn = int.tryParse(b);
      final int c;
      if (an != null && bn != null) {
        c = an.compareTo(bn);
      } else if (an != null) {
        c = -1;
      } else if (bn != null) {
        c = 1;
      } else {
        c = a.compareTo(b);
      }
      if (c != 0) {
        return c;
      }
    }
    return pre.length.compareTo(other.pre.length);
  }

  @override
  bool operator ==(Object other) => other is AppVersion && compareTo(other) == 0;

  @override
  int get hashCode => Object.hash(major, minor, patch, pre.join('.'));

  @override
  String toString() => '$major.$minor.$patch${pre.isEmpty ? '' : '-${pre.join('.')}'}';
}

/// A stable install ignores candidate pre-releases; a pre-release install may
/// move to a newer pre-release or to stable.
bool isNewer(AppVersion candidate, AppVersion current) {
  if (!current.isPrerelease) {
    return !candidate.isPrerelease && candidate.compareTo(current) > 0;
  }
  return candidate.compareTo(current) > 0;
}

class ReleaseAsset {
  const ReleaseAsset({required this.name, required this.size, required this.url});
  final String name;
  final int size;
  final String url;
}

class ReleaseInfo {
  const ReleaseInfo({
    required this.tag,
    required this.notes,
    required this.assets,
    this.prerelease = false,
    this.draft = false,
  });
  final String tag;
  final String notes;
  final List<ReleaseAsset> assets;
  final bool prerelease;
  final bool draft;

  /// Throws [UpdateException] when [json] is not a release object.
  static ReleaseInfo parse(String json) {
    final Object? value;
    try {
      value = jsonDecode(json);
    } on FormatException {
      throw const UpdateException(UpdateErrorKind.invalidRelease);
    }
    if (value is! Map<String, dynamic>) {
      throw const UpdateException(UpdateErrorKind.invalidRelease);
    }
    final assets = <ReleaseAsset>[];
    final rawAssets = value['assets'];
    if (rawAssets is List) {
      for (final a in rawAssets) {
        if (a is Map<String, dynamic>) {
          final name = a['name'];
          final size = a['size'];
          final url = a['browser_download_url'];
          if (name is String && size is int && url is String) {
            assets.add(ReleaseAsset(name: name, size: size, url: url));
          }
        }
      }
    }
    final tag = value['tag_name'];
    final body = value['body'];
    return ReleaseInfo(
      tag: tag is String ? tag : '',
      notes: sanitizeNotes(body is String ? body : ''),
      assets: assets,
      prerelease: value['prerelease'] == true,
      draft: value['draft'] == true,
    );
  }
}

/// Release notes are untrusted text: strip control characters and cap length.
String sanitizeNotes(String raw) {
  final cleaned = raw
      .replaceAll('\r\n', '\n')
      .replaceAll(RegExp(r'[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]'), '')
      .trim();
  if (cleaned.length <= maxNotesChars) {
    return cleaned;
  }
  return '${cleaned.substring(0, maxNotesChars)}...';
}

/// Only HTTPS URLs on the allowlisted GitHub hosts, without credentials,
/// non-default ports, fragments, or path traversal.
bool isAllowedAssetUrl(String raw, {String repo = releasesRepo}) {
  final Uri uri;
  try {
    uri = Uri.parse(raw);
  } on FormatException {
    return false;
  }
  if (uri.scheme != 'https' ||
      !allowedAssetHosts.contains(uri.host) ||
      uri.userInfo.isNotEmpty ||
      uri.hasFragment ||
      (uri.hasPort && uri.port != 443) ||
      uri.path.contains('..') ||
      uri.path.contains('//')) {
    return false;
  }
  if (uri.host == 'github.com') {
    // Initial asset URLs must be this repository's release downloads.
    return uri.path.startsWith('/$repo/releases/download/');
  }
  return true; // CDN hosts are only reached through a validated redirect hop
}

/// The APK and its checksum file for [release], by exact name.
({ReleaseAsset apk, ReleaseAsset checksum})? selectAssets(ReleaseInfo release) {
  final apkName = 'serabot-mobile-${release.tag}.apk';
  final sumName = '$apkName.sha256';
  ReleaseAsset? apk;
  ReleaseAsset? sum;
  for (final a in release.assets) {
    if (a.name == apkName &&
        a.size > 0 &&
        a.size <= maxApkBytes &&
        isAllowedAssetUrl(a.url)) {
      apk = a;
    } else if (a.name == sumName &&
        a.size > 0 &&
        a.size <= maxChecksumBytes &&
        isAllowedAssetUrl(a.url)) {
      sum = a;
    }
  }
  if (apk == null || sum == null) {
    return null;
  }
  return (apk: apk, checksum: sum);
}

/// Parse a `.sha256` file: `<64 hex>  <name>` (sha256sum style) or a bare
/// digest. When a name is present it must equal [expectedName].
List<int>? parseChecksum(String text, String expectedName) {
  for (final raw in text.split('\n')) {
    final line = raw.trim();
    if (line.isEmpty || line.startsWith('#')) {
      continue;
    }
    final match = RegExp(r'^([0-9a-fA-F]{64})(?:\s+\*?(\S+))?$').firstMatch(line);
    if (match == null) {
      return null;
    }
    final name = match.group(2);
    if (name != null && name != expectedName) {
      continue;
    }
    final hex = match.group(1)!;
    return [
      for (var i = 0; i < 64; i += 2) int.parse(hex.substring(i, i + 2), radix: 16),
    ];
  }
  return null;
}

bool digestsEqual(List<int> a, List<int> b) {
  if (a.length != b.length) {
    return false;
  }
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff == 0;
}

/// A newer release whose assets passed selection. The APK is not downloaded
/// until the user asks for it.
class AvailableUpdate {
  const AvailableUpdate({
    required this.version,
    required this.tag,
    required this.notes,
    required this.apk,
    required this.checksum,
  });
  final AppVersion version;
  final String tag;
  final String notes;
  final ReleaseAsset apk;
  final ReleaseAsset checksum;
}

class UpdateChecker {
  UpdateChecker({
    required this.current,
    required this.cacheDir,
    http.Client? client,
    this.repo = releasesRepo,
    this.apiBase = 'https://api.github.com',
  }) : _client = client ?? http.Client();

  final AppVersion current;
  final Directory cacheDir;
  final String repo;
  final String apiBase;
  final http.Client _client;

  /// Null when the app is up to date.
  Future<AvailableUpdate?> check() async {
    final body = await _getText(
      Uri.parse('$apiBase/repos/$repo/releases/latest'),
      maxReleaseJson,
      headers: const {'accept': 'application/vnd.github+json'},
      allowlist: false,
    );
    final release = ReleaseInfo.parse(body);
    if (release.draft || release.prerelease) {
      return null;
    }
    final version = AppVersion.parse(release.tag);
    if (version == null) {
      throw const UpdateException(UpdateErrorKind.invalidRelease);
    }
    if (!isNewer(version, current)) {
      return null;
    }
    final assets = selectAssets(release);
    if (assets == null) {
      throw const UpdateException(UpdateErrorKind.invalidRelease);
    }
    return AvailableUpdate(
      version: version,
      tag: release.tag,
      notes: release.notes,
      apk: assets.apk,
      checksum: assets.checksum,
    );
  }

  /// Download, hash-verify, and return the APK file. A file that fails
  /// verification is deleted and never returned.
  Future<File> download(
    AvailableUpdate update, {
    void Function(int received, int total)? onProgress,
  }) async {
    final sums = await _getText(Uri.parse(update.checksum.url), maxChecksumBytes);
    final expected = parseChecksum(sums, update.apk.name);
    if (expected == null) {
      throw const UpdateException(UpdateErrorKind.invalidRelease);
    }
    try {
      await cacheDir.create(recursive: true);
      // Clear older downloads so the cache holds at most one APK.
      await for (final entry in cacheDir.list()) {
        if (entry is File) {
          await entry.delete();
        }
      }
    } on FileSystemException {
      throw const UpdateException(UpdateErrorKind.storage);
    }
    final target = File('${cacheDir.path}/${update.apk.name}');
    final partial = File('${target.path}.part');
    final response = await _send(Uri.parse(update.apk.url), timeout: _downloadTimeout);
    final declared = response.contentLength;
    if (declared != null && declared > maxApkBytes) {
      throw const UpdateException(UpdateErrorKind.tooLarge);
    }
    final digestSink = _DigestCollector();
    final hasher = sha256.startChunkedConversion(digestSink);
    var received = 0;
    final out = partial.openWrite();
    try {
      await for (final chunk in response.stream.timeout(_downloadTimeout)) {
        received += chunk.length;
        if (received > maxApkBytes) {
          throw const UpdateException(UpdateErrorKind.tooLarge);
        }
        hasher.add(chunk);
        out.add(chunk);
        onProgress?.call(received, declared ?? update.apk.size);
      }
      await out.flush();
      await out.close();
    } on UpdateException {
      await _discard(out, partial);
      rethrow;
    } on TimeoutException {
      await _discard(out, partial);
      throw const UpdateException(UpdateErrorKind.network);
    } on http.ClientException {
      await _discard(out, partial);
      throw const UpdateException(UpdateErrorKind.network);
    } on FileSystemException {
      await _discard(out, partial);
      throw const UpdateException(UpdateErrorKind.storage);
    }
    hasher.close();
    if (!digestsEqual(digestSink.value!.bytes, expected)) {
      await partial.delete();
      throw const UpdateException(UpdateErrorKind.hashMismatch);
    }
    await partial.rename(target.path);
    return target;
  }

  Future<void> _discard(IOSink out, File partial) async {
    try {
      await out.close();
    } on Object {
      // already failing
    }
    try {
      if (await partial.exists()) {
        await partial.delete();
      }
    } on Object {
      // best effort
    }
  }

  Future<String> _getText(
    Uri uri,
    int limit, {
    Map<String, String> headers = const {},
    bool allowlist = true,
  }) async {
    final response = await _send(
      uri,
      headers: headers,
      timeout: _apiTimeout,
      allowlist: allowlist,
    );
    final bytes = <int>[];
    try {
      await for (final chunk in response.stream.timeout(_apiTimeout)) {
        bytes.addAll(chunk);
        if (bytes.length > limit) {
          throw const UpdateException(UpdateErrorKind.tooLarge);
        }
      }
    } on TimeoutException {
      throw const UpdateException(UpdateErrorKind.network);
    } on http.ClientException {
      throw const UpdateException(UpdateErrorKind.network);
    }
    return utf8.decode(bytes, allowMalformed: true);
  }

  /// GET with manual redirects so every hop is checked against the allowlist.
  Future<http.StreamedResponse> _send(
    Uri uri, {
    Map<String, String> headers = const {},
    required Duration timeout,
    bool allowlist = true,
  }) async {
    var target = uri;
    for (var hop = 0; hop <= 5; hop++) {
      if (allowlist
          ? !isAllowedAssetUrl(target.toString(), repo: repo)
          : target.scheme != 'https') {
        throw const UpdateException(UpdateErrorKind.untrustedUrl);
      }
      final request = http.Request('GET', target)
        ..followRedirects = false
        ..headers.addAll({'user-agent': 'serabot-mobile', ...headers});
      final http.StreamedResponse response;
      try {
        response = await _client.send(request).timeout(timeout);
      } on TimeoutException {
        throw const UpdateException(UpdateErrorKind.network);
      } on http.ClientException {
        throw const UpdateException(UpdateErrorKind.network);
      } on SocketException {
        throw const UpdateException(UpdateErrorKind.network);
      }
      final code = response.statusCode;
      if (code >= 300 && code < 400) {
        final location = response.headers['location'];
        await response.stream.drain<void>();
        if (location == null) {
          throw const UpdateException(UpdateErrorKind.invalidRelease);
        }
        target = target.resolve(location);
        // Redirect targets are always held to the host allowlist.
        allowlist = true;
        continue;
      }
      if (code != 200) {
        await response.stream.drain<void>();
        throw UpdateException(UpdateErrorKind.http, code);
      }
      return response;
    }
    throw const UpdateException(UpdateErrorKind.untrustedUrl);
  }

  void close() => _client.close();
}

class _DigestCollector implements Sink<Digest> {
  Digest? value;
  @override
  void add(Digest data) => value = data;
  @override
  void close() {}
}
