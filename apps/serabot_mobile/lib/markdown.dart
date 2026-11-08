import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Small, dependency-free markdown for chat replies: headings, bullet and
/// numbered lists, quotes, fenced code, **bold**, *italic*, `code` and
/// [links](https://...). Model output is untrusted: nothing is executed and a
/// link is only copied to the clipboard on tap, never opened.
enum MdBlockKind { paragraph, heading, bullet, number, quote, code }

class MdBlock {
  const MdBlock(this.kind, this.text, {this.level = 0, this.index = 0});
  final MdBlockKind kind;
  final String text;
  final int level;
  final int index;
}

List<MdBlock> parseMarkdownBlocks(String source) {
  final lines = source.replaceAll('\r\n', '\n').split('\n');
  final out = <MdBlock>[];
  final para = <String>[];
  void flush() {
    if (para.isNotEmpty) {
      out.add(MdBlock(MdBlockKind.paragraph, para.join('\n')));
      para.clear();
    }
  }

  var i = 0;
  while (i < lines.length) {
    final line = lines[i];
    if (line.trimLeft().startsWith('```')) {
      flush();
      final code = <String>[];
      i++;
      while (i < lines.length && !lines[i].trimLeft().startsWith('```')) {
        code.add(lines[i]);
        i++;
      }
      out.add(MdBlock(MdBlockKind.code, code.join('\n')));
      i++;
      continue;
    }
    final h = RegExp(r'^(#{1,6})\s+(.*)$').firstMatch(line);
    final b = RegExp(r'^\s*[-*+]\s+(.*)$').firstMatch(line);
    final n = RegExp(r'^\s*(\d{1,3})[.)]\s+(.*)$').firstMatch(line);
    final q = RegExp(r'^\s*>\s?(.*)$').firstMatch(line);
    if (h != null) {
      flush();
      out.add(MdBlock(MdBlockKind.heading, h.group(2)!, level: h.group(1)!.length));
    } else if (b != null) {
      flush();
      out.add(MdBlock(MdBlockKind.bullet, b.group(1)!));
    } else if (n != null) {
      flush();
      out.add(MdBlock(MdBlockKind.number, n.group(2)!,
          index: int.parse(n.group(1)!)));
    } else if (q != null) {
      flush();
      out.add(MdBlock(MdBlockKind.quote, q.group(1)!));
    } else if (line.trim().isEmpty) {
      flush();
    } else {
      para.add(line);
    }
    i++;
  }
  flush();
  return out;
}

class MdInline {
  const MdInline(this.text, {this.bold = false, this.italic = false, this.code = false, this.link});
  final String text;
  final bool bold;
  final bool italic;
  final bool code;
  final String? link;
}

final _inlineRe = RegExp(
  r'(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(__[^_\n]+__)|(\*[^*\s][^*\n]*\*)|(\[[^\]\n]+\]\(https?://[^)\s]+\))',
);

List<MdInline> parseInline(String s) {
  final out = <MdInline>[];
  var pos = 0;
  for (final m in _inlineRe.allMatches(s)) {
    if (m.start > pos) {
      out.add(MdInline(s.substring(pos, m.start)));
    }
    final t = m.group(0)!;
    if (m.group(1) != null) {
      out.add(MdInline(t.substring(1, t.length - 1), code: true));
    } else if (m.group(2) != null || m.group(3) != null) {
      out.add(MdInline(t.substring(2, t.length - 2), bold: true));
    } else if (m.group(4) != null) {
      out.add(MdInline(t.substring(1, t.length - 1), italic: true));
    } else {
      final close = t.indexOf('](');
      out.add(MdInline(t.substring(1, close),
          link: t.substring(close + 2, t.length - 1)));
    }
    pos = m.end;
  }
  if (pos < s.length) {
    out.add(MdInline(s.substring(pos)));
  }
  return out;
}

class MarkdownText extends StatelessWidget {
  const MarkdownText(this.data, {super.key, this.style});
  final String data;
  final TextStyle? style;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final base = style ?? theme.textTheme.bodyMedium!;
    final blocks = parseMarkdownBlocks(data);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final b in blocks)
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: _block(context, b, base),
          ),
      ],
    );
  }

  Widget _block(BuildContext context, MdBlock b, TextStyle base) {
    final scheme = Theme.of(context).colorScheme;
    switch (b.kind) {
      case MdBlockKind.code:
        return Container(
          width: double.infinity,
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(
            color: scheme.surfaceContainerHighest,
            borderRadius: BorderRadius.circular(10),
          ),
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: SelectableText(
              b.text,
              style: base.copyWith(fontFamily: 'monospace', fontSize: 13),
            ),
          ),
        );
      case MdBlockKind.heading:
        final size = const <double>[0, 22, 20, 18, 16, 15, 14][b.level];
        return _rich(context, b.text,
            base.copyWith(fontSize: size, fontWeight: FontWeight.w700));
      case MdBlockKind.bullet:
      case MdBlockKind.number:
        final marker = b.kind == MdBlockKind.bullet ? '\u2022' : '${b.index}.';
        return Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(width: 24, child: Text(marker, style: base)),
            Expanded(child: _rich(context, b.text, base)),
          ],
        );
      case MdBlockKind.quote:
        return Container(
          padding: const EdgeInsets.only(left: 10),
          decoration: BoxDecoration(
            border: Border(left: BorderSide(color: scheme.primary, width: 3)),
          ),
          child: _rich(context, b.text,
              base.copyWith(color: scheme.onSurfaceVariant)),
        );
      case MdBlockKind.paragraph:
        return _rich(context, b.text, base);
    }
  }

  Widget _rich(BuildContext context, String text, TextStyle base) {
    final scheme = Theme.of(context).colorScheme;
    final spans = <InlineSpan>[];
    for (final p in parseInline(text)) {
      var s = base;
      if (p.bold) {
        s = s.copyWith(fontWeight: FontWeight.w700);
      }
      if (p.italic) {
        s = s.copyWith(fontStyle: FontStyle.italic);
      }
      if (p.code) {
        s = s.copyWith(
          fontFamily: 'monospace',
          backgroundColor: scheme.surfaceContainerHighest,
        );
      }
      if (p.link != null) {
        final url = p.link!;
        s = s.copyWith(
          color: scheme.primary,
          decoration: TextDecoration.underline,
        );
        spans.add(TextSpan(
          text: p.text,
          style: s,
          recognizer: TapGestureRecognizer()
            ..onTap = () async {
              await Clipboard.setData(ClipboardData(text: url));
              if (context.mounted) {
                ScaffoldMessenger.maybeOf(context)?.showSnackBar(
                  const SnackBar(content: Text('Link copied')),
                );
              }
            },
        ));
      } else {
        spans.add(TextSpan(text: p.text, style: s));
      }
    }
    return Text.rich(TextSpan(children: spans, style: base));
  }
}
