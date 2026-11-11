import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/chat_scroll.dart';

void main() {
  test('near bottom uses the shared 80 px threshold', () {
    expect(nearBottom(0), isTrue);
    expect(nearBottom(79.9), isTrue);
    expect(nearBottom(80), isFalse);
    expect(nearBottom(400), isFalse);
  });

  test('badge caps at 99+ and hides at zero', () {
    expect(unseenBadge(0), isNull);
    expect(unseenBadge(-1), isNull);
    expect(unseenBadge(3), '3');
    expect(unseenBadge(99), '99');
    expect(unseenBadge(100), '99+');
  });

  test('unseen counts new messages only while scrolled away', () {
    expect(nextUnseen(unseen: 2, previousCount: 5, count: 6, atBottom: true), 0);
    expect(nextUnseen(unseen: 0, previousCount: 5, count: 7, atBottom: false), 2);
    expect(nextUnseen(unseen: 2, previousCount: 7, count: 8, atBottom: false), 3);
    expect(nextUnseen(unseen: 2, previousCount: 8, count: 8, atBottom: false), 2);
    expect(nextUnseen(unseen: 2, previousCount: 8, count: 5, atBottom: false), 2);
  });
}
