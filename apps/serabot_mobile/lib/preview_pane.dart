import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:material_ui/material_ui.dart';

import 'api_client.dart';
import 'chat_models.dart';
import 'markdown.dart';

/// Parsed sheet from extracted `.xlsx`/`.xls`/`.csv`/`.tsv` preview text.
class SpreadsheetSheet {
  const SpreadsheetSheet({
    required this.name,
    required this.headers,
    required this.rows,
  });

  final String name;
  final List<String> headers;
  final List<List<String>> rows;
}

/// Parses `=== Sheet: Name ===` blocks and TSV/CSV rows into [SpreadsheetSheet]s.
List<SpreadsheetSheet> parseSpreadsheetSheets(String text, {String? filename}) {
  final lines = const LineSplitter().convert(text);
  final sections = <({String name, List<String> lines})>[];
  var currentName = 'Sheet 1';
  var currentLines = <String>[];
  final headerRe = RegExp(r'^===\s*Sheet:\s*(.+?)\s*===$', caseSensitive: false);

  for (final line in lines) {
    final m = headerRe.firstMatch(line.trim());
    if (m != null) {
      if (currentLines.isNotEmpty) {
        sections.add((name: currentName, lines: currentLines));
      }
      currentName = m.group(1)?.trim().isNotEmpty == true
          ? m.group(1)!.trim()
          : 'Sheet ${sections.length + 1}';
      currentLines = <String>[];
    } else {
      currentLines.add(line);
    }
  }
  if (currentLines.isNotEmpty || sections.isEmpty) {
    sections.add((name: currentName, lines: currentLines));
  }

  final preferTsv = filename?.toLowerCase().endsWith('.tsv') == true;
  final out = <SpreadsheetSheet>[];
  for (final sec in sections) {
    final nonEmpty = sec.lines.where((l) => l.trim().isNotEmpty).toList();
    if (nonEmpty.isEmpty) {
      continue;
    }
    final useTab = preferTsv || nonEmpty.any((l) => l.contains('\t'));
    final parsedRows = nonEmpty
        .map((l) => useTab ? l.split('\t') : _splitCsvLine(l))
        .toList();
    final maxCols = parsedRows.fold<int>(
        0, (best, r) => r.length > best ? r.length : best);
    if (maxCols == 0) {
      continue;
    }
    List<String> pad(List<String> row) => [
          for (var i = 0; i < maxCols; i++)
            i < row.length ? row[i].trim() : '',
        ];
    final headers = pad(parsedRows.first);
    final rows = parsedRows.skip(1).map(pad).toList();
    out.add(SpreadsheetSheet(name: sec.name, headers: headers, rows: rows));
  }
  return out;
}

List<String> _splitCsvLine(String line) {
  final out = <String>[];
  final buf = StringBuffer();
  var inQuotes = false;
  for (var i = 0; i < line.length; i++) {
    final ch = line[i];
    if (ch == '"') {
      if (inQuotes && i + 1 < line.length && line[i + 1] == '"') {
        buf.write('"');
        i++;
      } else {
        inQuotes = !inQuotes;
      }
    } else if (ch == ',' && !inQuotes) {
      out.add(buf.toString());
      buf.clear();
    } else {
      buf.write(ch);
    }
  }
  out.add(buf.toString());
  return out;
}

bool isNumericCell(String raw) {
  final t = raw.trim();
  if (t.isEmpty) return false;
  final cleaned = t.replaceAll(RegExp(r'^[$€£¥+\-\s]+|[%,\s]+$'), '');
  return double.tryParse(cleaned.replaceAll(',', '')) != null;
}

/// Parsed slide from `.pptx` preview text.
class PresentationSlide {
  const PresentationSlide({
    required this.number,
    required this.title,
    required this.lines,
  });

  final int number;
  final String title;
  final List<String> lines;
}

List<PresentationSlide> parsePresentationSlides(String text) {
  final rawLines = const LineSplitter().convert(text);
  final slides = <PresentationSlide>[];
  final markerRe =
      RegExp(r'^(?:===\s*)?Slide\s+(\d+)\s*(?::\s*(.*?))?(?:===)?$',
          caseSensitive: false);
  var currentNum = 1;
  var currentTitle = '';
  var body = <String>[];

  void flush() {
    if (currentTitle.isEmpty && body.isEmpty) return;
    final title = currentTitle.isNotEmpty
        ? currentTitle
        : (body.isNotEmpty ? body.removeAt(0) : 'Slide $currentNum');
    slides.add(PresentationSlide(
      number: currentNum,
      title: title,
      lines: body.where((l) => l.trim().isNotEmpty).toList(),
    ));
  }

  for (final line in rawLines) {
    final trimmed = line.trim();
    final m = markerRe.firstMatch(trimmed);
    if (m != null) {
      flush();
      currentNum = int.tryParse(m.group(1) ?? '') ?? (slides.length + 1);
      currentTitle = m.group(2)?.trim() ?? '';
      body = <String>[];
    } else if (trimmed == '---') {
      flush();
      currentNum = slides.length + 1;
      currentTitle = '';
      body = <String>[];
    } else {
      body.add(trimmed);
    }
  }
  flush();
  return slides;
}

/// Parsed block for `.docx` document reader.
class DocumentBlock {
  const DocumentBlock({
    required this.text,
    this.headingLevel = 0,
    this.isBullet = false,
  });

  final String text;
  final int headingLevel;
  final bool isBullet;
}

List<DocumentBlock> parseDocumentBlocks(String text) {
  final lines = const LineSplitter().convert(text);
  final blocks = <DocumentBlock>[];
  for (final raw in lines) {
    final t = raw.trim();
    if (t.isEmpty) continue;
    if (t.startsWith('### ')) {
      blocks.add(DocumentBlock(text: t.substring(4).trim(), headingLevel: 3));
    } else if (t.startsWith('## ')) {
      blocks.add(DocumentBlock(text: t.substring(3).trim(), headingLevel: 2));
    } else if (t.startsWith('# ')) {
      blocks.add(DocumentBlock(text: t.substring(2).trim(), headingLevel: 1));
    } else if (t.startsWith('- ') || t.startsWith('* ') || t.startsWith('• ')) {
      blocks.add(DocumentBlock(text: t.substring(2).trim(), isBullet: true));
    } else {
      blocks.add(DocumentBlock(text: t));
    }
  }
  return blocks;
}

enum _PreviewMode { pdf, sheet, slide, doc, image, markdown, code }

_PreviewMode _resolveMode(String name, ArtifactPreview preview) {
  final lower = name.trim().toLowerCase();
  final dot = lower.lastIndexOf('.');
  final ext = (dot >= 0 && dot < lower.length - 1) ? lower.substring(dot + 1) : '';
  switch (ext) {
    case 'pdf':
      return _PreviewMode.pdf;
    case 'xlsx':
    case 'xls':
    case 'csv':
    case 'tsv':
      return _PreviewMode.sheet;
    case 'pptx':
      return _PreviewMode.slide;
    case 'docx':
      return _PreviewMode.doc;
    case 'png':
    case 'jpg':
    case 'jpeg':
    case 'gif':
    case 'webp':
      return _PreviewMode.image;
    case 'md':
    case 'markdown':
      return _PreviewMode.markdown;
    default:
      if (preview.imageB64 != null &&
          preview.contentType.startsWith('image/')) {
        return _PreviewMode.image;
      }
      return _PreviewMode.code;
  }
}

/// A server-rendered preview pane for one produced file (`GET /v1/artifacts/{hash}/preview`).
class ArtifactPreviewPane extends StatefulWidget {
  const ArtifactPreviewPane({
    super.key,
    required this.client,
    required this.entry,
    this.onFallback,
    this.fullscreen = false,
  });

  final ApiClient client;
  final LibraryEntry entry;
  final VoidCallback? onFallback;
  final bool fullscreen;

  @override
  State<ArtifactPreviewPane> createState() => _ArtifactPreviewPaneState();
}

class _ArtifactPreviewPaneState extends State<ArtifactPreviewPane> {
  late Future<ArtifactPreview?> _future;
  bool _pdfShowText = false;
  int _selectedSheet = 0;

  @override
  void initState() {
    super.initState();
    _future = widget.client.artifactPreview(widget.entry.hash);
  }

  @override
  Widget build(BuildContext context) {
    final kind = previewKindForName(widget.entry.label);
    return FutureBuilder<ArtifactPreview?>(
      future: _future,
      builder: (context, snap) {
        if (snap.connectionState != ConnectionState.done) {
          return const Padding(
            padding: EdgeInsets.all(32),
            child: Center(child: CircularProgressIndicator()),
          );
        }
        final preview = snap.data;
        final scheme = Theme.of(context).colorScheme;
        if (snap.hasError || preview == null || !preview.hasContent) {
          return Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(Icons.insert_drive_file_outlined,
                    size: 40, color: scheme.outline),
                const SizedBox(height: 8),
                Text(
                  'No preview available for this file.',
                  style: Theme.of(context).textTheme.bodyMedium,
                  textAlign: TextAlign.center,
                ),
                if (widget.onFallback != null) ...[
                  const SizedBox(height: 12),
                  FilledButton.icon(
                    onPressed: widget.onFallback,
                    icon: const Icon(Icons.share_outlined),
                    label: const Text('Open / share'),
                  ),
                ],
              ],
            ),
          );
        }

        final mode = _resolveMode(widget.entry.label, preview);
        switch (mode) {
          case _PreviewMode.pdf:
            return _buildPdf(context, preview, kind);
          case _PreviewMode.sheet:
            return _buildSheet(context, preview);
          case _PreviewMode.slide:
            return _buildSlides(context, preview);
          case _PreviewMode.doc:
            return _buildDoc(context, preview);
          case _PreviewMode.image:
            return _buildImage(context, preview);
          case _PreviewMode.markdown:
            return _buildMarkdown(context, preview);
          case _PreviewMode.code:
            return _buildCode(context, preview);
        }
      },
    );
  }

  Widget _buildPdf(
      BuildContext context, ArtifactPreview preview, PreviewKind? kind) {
    final hasImage = preview.imageB64 != null;
    final hasText = preview.text != null && preview.text!.isNotEmpty;
    final showText = (!hasImage) || (_pdfShowText && hasText);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
          child: Row(
            children: [
              Chip(
                avatar: const Icon(Icons.picture_as_pdf_outlined, size: 16),
                label: Text(kind?.label ?? 'PDF'),
                visualDensity: VisualDensity.compact,
              ),
              const Spacer(),
              if (hasImage && hasText)
                SegmentedButton<bool>(
                  segments: const [
                    ButtonSegment(value: false, label: Text('Visual Page')),
                    ButtonSegment(value: true, label: Text('Extracted Text')),
                  ],
                  selected: {_pdfShowText},
                  onSelectionChanged: (s) =>
                      setState(() => _pdfShowText = s.first),
                ),
            ],
          ),
        ),
        Flexible(
          child: showText
              ? SingleChildScrollView(
                  padding: const EdgeInsets.all(16),
                  child: SelectableText(
                    preview.text!,
                    style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                          height: 1.5,
                        ),
                  ),
                )
              : InteractiveViewer(
                  minScale: 0.8,
                  maxScale: 5,
                  child: Image.memory(
                    base64Decode(preview.imageB64!),
                    fit: BoxFit.contain,
                  ),
                ),
        ),
      ],
    );
  }

  Widget _buildSheet(BuildContext context, ArtifactPreview preview) {
    final text = preview.text ?? '';
    final sheets =
        parseSpreadsheetSheets(text, filename: widget.entry.label);
    if (sheets.isEmpty) {
      return _buildCode(context, preview);
    }
    final idx = _selectedSheet.clamp(0, sheets.length - 1);
    final sheet = sheets[idx];
    final scheme = Theme.of(context).colorScheme;
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (sheets.length > 1)
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
            child: Row(
              children: [
                for (var i = 0; i < sheets.length; i++) ...[
                  ChoiceChip(
                    selected: i == idx,
                    label: Text(sheets[i].name),
                    onSelected: (_) => setState(() => _selectedSheet = i),
                  ),
                  const SizedBox(width: 8),
                ],
              ],
            ),
          ),
        Flexible(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(12),
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: DataTable(
                headingRowColor:
                    WidgetStatePropertyAll(scheme.surfaceContainerHighest),
                border: TableBorder.all(
                  color: scheme.outlineVariant.withValues(alpha: 0.5),
                  borderRadius: BorderRadius.circular(8),
                ),
                columns: [
                  for (var c = 0; c < sheet.headers.length; c++)
                    DataColumn(
                      numeric: sheet.rows.isNotEmpty &&
                          sheet.rows.every(
                              (r) => r[c].isEmpty || isNumericCell(r[c])),
                      label: Text(
                        sheet.headers[c],
                        style: const TextStyle(fontWeight: FontWeight.w700),
                      ),
                    ),
                ],
                rows: [
                  for (final r in sheet.rows)
                    DataRow(
                      cells: [
                        for (final cell in r) DataCell(Text(cell)),
                      ],
                    ),
                ],
              ),
            ),
          ),
        ),
      ],
    );
  }

  Widget _buildSlides(BuildContext context, ArtifactPreview preview) {
    final slides = parsePresentationSlides(preview.text ?? '');
    if (slides.isEmpty) {
      return _buildCode(context, preview);
    }
    final scheme = Theme.of(context).colorScheme;
    return ListView.separated(
      shrinkWrap: !widget.fullscreen,
      padding: const EdgeInsets.all(16),
      itemCount: slides.length,
      separatorBuilder: (context, _) => const SizedBox(height: 12),
      itemBuilder: (context, i) {
        final slide = slides[i];
        return Card(
          elevation: 0,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(14),
            side: BorderSide(color: scheme.outlineVariant),
          ),
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Container(
                  padding:
                      const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                  decoration: BoxDecoration(
                    color: scheme.primaryContainer,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Text(
                    'Slide ${slide.number}',
                    style: Theme.of(context).textTheme.labelSmall?.copyWith(
                          color: scheme.onPrimaryContainer,
                          fontWeight: FontWeight.w700,
                        ),
                  ),
                ),
                const SizedBox(height: 10),
                Text(
                  slide.title,
                  style: Theme.of(context).textTheme.titleMedium?.copyWith(
                        fontWeight: FontWeight.w700,
                      ),
                ),
                if (slide.lines.isNotEmpty) const SizedBox(height: 8),
                for (final line in slide.lines)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 4),
                    child: Text(
                      line,
                      style: Theme.of(context).textTheme.bodyMedium,
                    ),
                  ),
              ],
            ),
          ),
        );
      },
    );
  }

  Widget _buildDoc(BuildContext context, ArtifactPreview preview) {
    final blocks = parseDocumentBlocks(preview.text ?? '');
    if (blocks.isEmpty) {
      return _buildCode(context, preview);
    }
    return ListView.builder(
      shrinkWrap: !widget.fullscreen,
      padding: const EdgeInsets.all(16),
      itemCount: blocks.length,
      itemBuilder: (context, i) {
        final b = blocks[i];
        final theme = Theme.of(context).textTheme;
        if (b.headingLevel == 1) {
          return Padding(
            padding: const EdgeInsets.only(top: 8, bottom: 6),
            child: Text(b.text,
                style:
                    theme.titleLarge?.copyWith(fontWeight: FontWeight.w700)),
          );
        }
        if (b.headingLevel == 2) {
          return Padding(
            padding: const EdgeInsets.only(top: 10, bottom: 4),
            child: Text(b.text,
                style:
                    theme.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
          );
        }
        if (b.headingLevel >= 3) {
          return Padding(
            padding: const EdgeInsets.only(top: 8, bottom: 4),
            child: Text(b.text,
                style:
                    theme.titleSmall?.copyWith(fontWeight: FontWeight.w600)),
          );
        }
        if (b.isBullet) {
          return Padding(
            padding: const EdgeInsets.only(left: 8, bottom: 4),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text('•  '),
                Expanded(child: Text(b.text, style: theme.bodyMedium)),
              ],
            ),
          );
        }
        return Padding(
          padding: const EdgeInsets.only(bottom: 8),
          child: Text(b.text, style: theme.bodyMedium?.copyWith(height: 1.45)),
        );
      },
    );
  }

  Widget _buildImage(BuildContext context, ArtifactPreview preview) {
    final b64 = preview.imageB64;
    if (b64 == null) {
      return _buildCode(context, preview);
    }
    return Container(
      color: const Color(0xFF111318),
      child: Stack(
        alignment: Alignment.center,
        children: [
          InteractiveViewer(
            minScale: 0.5,
            maxScale: 6,
            child: Image.memory(
              base64Decode(b64),
              fit: BoxFit.contain,
            ),
          ),
          if (widget.onFallback != null)
            Positioned(
              right: 12,
              bottom: 12,
              child: FilledButton.tonalIcon(
                onPressed: widget.onFallback,
                icon: const Icon(Icons.share_outlined, size: 18),
                label: const Text('Save / Share'),
              ),
            ),
        ],
      ),
    );
  }

  Widget _buildMarkdown(BuildContext context, ArtifactPreview preview) {
    final text = preview.text ?? '';
    return SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: MarkdownText(text),
    );
  }

  Widget _buildCode(BuildContext context, ArtifactPreview preview) {
    final text = preview.text ?? '';
    final scheme = Theme.of(context).colorScheme;
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
          child: Row(
            children: [
              Text(
                preview.contentType,
                style: Theme.of(context)
                    .textTheme
                    .labelSmall
                    ?.copyWith(color: scheme.onSurfaceVariant),
              ),
              const Spacer(),
              TextButton.icon(
                onPressed: () async {
                  final messenger = ScaffoldMessenger.of(context);
                  await Clipboard.setData(ClipboardData(text: text));
                  messenger.showSnackBar(
                    const SnackBar(content: Text('Copied to clipboard')),
                  );
                },
                icon: const Icon(Icons.copy_rounded, size: 16),
                label: const Text('Copy'),
              ),
            ],
          ),
        ),
        Flexible(
          child: SingleChildScrollView(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 16),
            child: Container(
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHighest.withValues(alpha: 0.5),
                borderRadius: BorderRadius.circular(10),
              ),
              child: SelectableText(
                text,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// Bottom sheet that hosts [ArtifactPreviewPane] with a full-screen toggle.
Future<void> showArtifactPreviewSheet(
  BuildContext context, {
  required ApiClient client,
  required LibraryEntry entry,
  VoidCallback? onFallback,
}) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (sheetCtx) => SafeArea(
      child: Padding(
        padding: const EdgeInsets.only(bottom: 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      entry.label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: Theme.of(sheetCtx).textTheme.titleMedium,
                    ),
                  ),
                  IconButton(
                    tooltip: 'Full screen',
                    icon: const Icon(Icons.fullscreen_rounded),
                    onPressed: () {
                      Navigator.of(sheetCtx).push(
                        MaterialPageRoute<void>(
                          fullscreenDialog: true,
                          builder: (_) => Scaffold(
                            appBar: AppBar(
                              title: Text(
                                entry.label,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              actions: [
                                if (onFallback != null)
                                  IconButton(
                                    tooltip: 'Share / save',
                                    icon: const Icon(Icons.share_outlined),
                                    onPressed: onFallback,
                                  ),
                              ],
                            ),
                            body: ArtifactPreviewPane(
                              client: client,
                              entry: entry,
                              onFallback: onFallback,
                              fullscreen: true,
                            ),
                          ),
                        ),
                      );
                    },
                  ),
                  if (onFallback != null)
                    IconButton(
                      tooltip: 'Share / save',
                      icon: const Icon(Icons.share_outlined),
                      onPressed: onFallback,
                    ),
                ],
              ),
            ),
            const SizedBox(height: 4),
            ConstrainedBox(
              constraints: BoxConstraints(
                maxHeight: MediaQuery.of(sheetCtx).size.height * 0.68,
              ),
              child: ArtifactPreviewPane(
                client: client,
                entry: entry,
                onFallback: onFallback,
              ),
            ),
          ],
        ),
      ),
    ),
  );
}
