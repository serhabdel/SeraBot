import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/validation.dart';

void main() {
  group('validateEndpoint', () {
    test('accepts https origin', () {
      expect(validateEndpoint('https://sera.example.com'), 'https://sera.example.com');
    });

    test('accepts loopback http', () {
      expect(
        validateEndpoint('http://127.0.0.1:8787'),
        'http://127.0.0.1:8787',
      );
    });

    test('rejects credentials', () {
      expect(
        () => validateEndpoint('https://user@host.example.com'),
        throwsA(isA<ValidationException>()),
      );
    });

    test('rejects path', () {
      expect(
        () => validateEndpoint('https://host.example.com/v1'),
        throwsA(isA<ValidationException>()),
      );
    });

    test('rejects plain http non-loopback', () {
      expect(
        () => validateEndpoint('http://example.com'),
        throwsA(isA<ValidationException>()),
      );
    });
  });

  group('validToken', () {
    test('length bounds', () {
      expect(validToken('x' * 31), isFalse);
      expect(validToken('x' * 32), isTrue);
      expect(validToken('x' * 512), isTrue);
      expect(validToken('x' * 513), isFalse);
    });
  });

  group('validId', () {
    test('allows android-owner style ids', () {
      expect(validId('android-owner'), isTrue);
      expect(validId('windows-owner'), isTrue);
    });
  });
}
