import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/markdown.dart';
import 'package:serabot_mobile/widgets.dart';

void main() {
  test('blocks: headings, lists, quotes, code fences, paragraphs', () {
    final b = parseMarkdownBlocks('# Title\n\nSome text\nmore\n\n- one\n- two\n1. first\n> quoted\n```\ncode here\n```\n');
    expect(b.map((e) => e.kind), [
      MdBlockKind.heading,
      MdBlockKind.paragraph,
      MdBlockKind.bullet,
      MdBlockKind.bullet,
      MdBlockKind.number,
      MdBlockKind.quote,
      MdBlockKind.code,
    ]);
    expect(b.first.level, 1);
    expect(b[1].text, 'Some text\nmore');
    expect(b[4].index, 1);
    expect(b.last.text, 'code here');
  });

  test('inline: bold, italic, code and links', () {
    final p = parseInline('a **b** *c* `d` [e](https://x.example/p) f');
    expect(p.where((e) => e.bold).single.text, 'b');
    expect(p.where((e) => e.italic).single.text, 'c');
    expect(p.where((e) => e.code).single.text, 'd');
    expect(p.where((e) => e.link != null).single.link, 'https://x.example/p');
    expect(p.map((e) => e.text).join(), 'a b c d e f');
  });

  test('javascript: links are not links', () {
    final p = parseInline('[x](javascript:alert(1))');
    expect(p.every((e) => e.link == null), isTrue);
  });

  test('shortTime: today, this week, older', () {
    final now = DateTime(2026, 5, 20, 12);
    expect(shortTime(DateTime(2026, 5, 20, 9, 5).toIso8601String(), now: now), '09:05');
    expect(shortTime(DateTime(2026, 5, 18, 9).toIso8601String(), now: now), 'Mon');
    expect(shortTime(DateTime(2026, 1, 3, 9).toIso8601String(), now: now), '3 Jan');
    expect(shortTime('garbage', now: now), '');
  });
}
