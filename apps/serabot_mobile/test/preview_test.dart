import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/api_client.dart';
import 'package:serabot_mobile/chat_models.dart';

void main() {
  test('previewKindForName covers the five formats only', () {
    expect(previewKindForName('deck.PPTX'), PreviewKind.slide);
    expect(previewKindForName('report.pdf'), PreviewKind.pdf);
    expect(previewKindForName('ledger.xlsx'), PreviewKind.sheet);
    expect(previewKindForName('notes.docx'), PreviewKind.doc);
    expect(previewKindForName('README.md'), PreviewKind.markdown);
    expect(previewKindForName('photo.png'), isNull);
    expect(previewKindForName('noextension'), isNull);
    expect(previewKindForName('trailing.'), isNull);
    expect(previewKindForName(''), isNull);
    for (final k in PreviewKind.values) {
      expect(k.label, isNotEmpty);
    }
  });

  test('ArtifactPreview parses and sanitizes text', () {
    final p = ArtifactPreview.fromJson({
      'content_type': 'text/markdown',
      'text': '# Title\n\x07body\ttext',
    });
    expect(p, isNotNull);
    expect(p!.contentType, 'text/markdown');
    expect(p.text, '# Title\nbody\ttext');
    expect(p.hasContent, isTrue);
    expect(p.imageDataUrl, isNull);
  });

  test('ArtifactPreview rejects malformed payloads', () {
    expect(ArtifactPreview.fromJson({}), isNull);
    expect(ArtifactPreview.fromJson({'content_type': ''}), isNull);
    expect(
      ArtifactPreview.fromJson({
        'content_type': 'text/plain',
        'image_b64': '!!not base64!!',
      })!.imageB64,
      isNull,
    );
    // Blank text counts as no content.
    final blank = ArtifactPreview.fromJson(
        {'content_type': 'text/plain', 'text': '   '});
    expect(blank!.hasContent, isFalse);
  });

  test('ArtifactPreview image becomes a data url, capped text', () {
    final p = ArtifactPreview.fromJson({
      'content_type': 'image/png',
      'image_b64': 'aGk=',
    })!;
    expect(p.imageDataUrl, 'data:image/png;base64,aGk=');
    // Non-image content type falls back to png.
    final other = ArtifactPreview.fromJson({
      'content_type': 'application/octet-stream',
      'image_b64': 'aGk=',
    })!;
    expect(other.imageDataUrl, startsWith('data:image/png;base64,'));
    final long = ArtifactPreview.fromJson({
      'content_type': 'text/plain',
      'text': 'x' * (ArtifactPreview.maxTextChars * 3),
    })!;
    expect(long.text!.runes.length, ArtifactPreview.maxTextChars);
  });

  group('artifactPreview against a local stub', () {
    HttpServer? server;
    tearDown(() async {
      await server?.close(force: true);
      server = null;
    });

    test('parses a preview payload', () async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      final port = server!.port;
      server!.listen((request) async {
        expect(request.uri.path, '/v1/artifacts/${'a' * 64}/preview');
        expect(request.headers.value('authorization'),
            'Bearer ${'a' * 32}');
        request.response
          ..statusCode = HttpStatus.ok
          ..write(jsonEncode({
            'content_type': 'image/png',
            'image_b64': 'aGk=',
          }));
        await request.response.close();
      });
      final client = ApiClient.connect(
          'http://127.0.0.1:$port', 'a' * 32);
      addTearDown(client.close);
      final preview = await client.artifactPreview('a' * 64);
      expect(preview, isNotNull);
      expect(preview!.imageDataUrl, 'data:image/png;base64,aGk=');
    });

    test('404 means no preview (fall back to download)', () async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      final port = server!.port;
      server!.listen((request) async {
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
      });
      final client = ApiClient.connect(
          'http://127.0.0.1:$port', 'a' * 32);
      addTearDown(client.close);
      expect(await client.artifactPreview('b' * 64), isNull);
    });

    test('an invalid hash is refused before any request', () async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      final port = server!.port;
      var hits = 0;
      server!.listen((request) async {
        hits++;
        request.response.statusCode = HttpStatus.ok;
        await request.response.close();
      });
      final client = ApiClient.connect(
          'http://127.0.0.1:$port', 'a' * 32);
      addTearDown(client.close);
      expect(
        () => client.artifactPreview('xyz'),
        throwsA(isA<ApiException>()),
      );
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(hits, 0);
    });
  });
}
