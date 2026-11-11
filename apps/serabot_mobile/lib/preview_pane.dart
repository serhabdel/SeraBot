import 'dart:convert';

import 'package:material_ui/material_ui.dart';

import 'api_client.dart';
import 'chat_models.dart';

/// A server-rendered preview pane for one produced file (0.28.0):
/// `GET /v1/artifacts/{hash}/preview`. Renders the image the server sent,
/// else the extracted text. When the server has no preview (older server,
/// 404) or the payload is unusable, the pane says so and [onFallback]
/// (download/share) stays the way out.
class ArtifactPreviewPane extends StatelessWidget {
  const ArtifactPreviewPane({
    super.key,
    required this.client,
    required this.entry,
    this.onFallback,
  });

  final ApiClient client;
  final LibraryEntry entry;
  final VoidCallback? onFallback;

  @override
  Widget build(BuildContext context) {
    final kind = previewKindForName(entry.label);
    return FutureBuilder<ArtifactPreview?>(
      future: client.artifactPreview(entry.hash),
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
                if (onFallback != null) ...[
                  const SizedBox(height: 12),
                  FilledButton.icon(
                    onPressed: onFallback,
                    icon: const Icon(Icons.share_outlined),
                    label: const Text('Open / share'),
                  ),
                ],
              ],
            ),
          );
        }
        final url = preview.imageDataUrl;
        if (url != null && preview.imageB64 != null) {
          return Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (kind != null)
                Padding(
                  padding:
                      const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: Text(kind.label,
                        style: Theme.of(context).textTheme.labelMedium),
                  ),
                ),
              Flexible(
                child: InteractiveViewer(
                  maxScale: 4,
                  child: Image.memory(
                    base64Decode(preview.imageB64!),
                    fit: BoxFit.contain,
                  ),
                ),
              ),
            ],
          );
        }
        return SingleChildScrollView(
          padding: const EdgeInsets.all(16),
          child: SelectableText(
            preview.text!,
            style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
          ),
        );
      },
    );
  }
}

/// Bottom sheet that hosts [ArtifactPreviewPane] for one library entry.
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
    builder: (_) => SafeArea(
      child: Padding(
        padding: const EdgeInsets.only(bottom: 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(
                  entry.label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
            ),
            const SizedBox(height: 4),
            ConstrainedBox(
              constraints: BoxConstraints(
                maxHeight: MediaQuery.of(context).size.height * 0.65,
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
