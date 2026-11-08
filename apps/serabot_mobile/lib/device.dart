import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:path_provider/path_provider.dart';
import 'package:record/record.dart';
import 'package:share_plus/share_plus.dart';

import 'chat_models.dart';

class PickedFile {
  const PickedFile(this.name, this.bytes);
  final String name;
  final List<int> bytes;
}

class RecordedAudio {
  const RecordedAudio(this.name, this.bytes);
  final String name;
  final List<int> bytes;
}

/// Phone features the chat needs (file picker, microphone, share sheet),
/// behind an interface so widget tests can run without platform plugins.
abstract class DeviceActions {
  /// Files or photos chosen by the user. Oversized or unsupported picks are
  /// skipped by the caller; this only reads the bytes.
  Future<List<PickedFile>> pickFiles({bool imagesOnly = false});

  /// False when microphone permission is missing.
  Future<bool> startRecording();
  Future<RecordedAudio?> stopRecording();
  Future<void> cancelRecording();

  /// Opens the system share sheet (also the way to save a file elsewhere).
  Future<void> shareFile(String filename, List<int> bytes);
}

class PlatformDeviceActions implements DeviceActions {
  AudioRecorder? _recorder;

  @override
  Future<List<PickedFile>> pickFiles({bool imagesOnly = false}) async {
    final picked = await FilePicker.pickFiles(
      type: FileType.custom,
      allowedExtensions: imagesOnly
          ? const ['png', 'jpg', 'jpeg', 'gif', 'webp']
          : attachmentExtensions,
    );
    final out = <PickedFile>[];
    for (final f in picked) {
      final len = await f.length();
      if (len != null && len > maxAttachmentBytes) {
        continue;
      }
      out.add(PickedFile(f.name, await f.readAsBytes()));
    }
    return out;
  }

  @override
  Future<bool> startRecording() async {
    final r = _recorder ??= AudioRecorder();
    if (!await r.hasPermission()) {
      return false;
    }
    final dir = await getTemporaryDirectory();
    final path =
        '${dir.path}${Platform.pathSeparator}voice-${DateTime.now().millisecondsSinceEpoch}.m4a';
    await r.start(const RecordConfig(encoder: AudioEncoder.aacLc), path: path);
    return true;
  }

  @override
  Future<RecordedAudio?> stopRecording() async {
    final path = await _recorder?.stop();
    if (path == null) {
      return null;
    }
    final file = File(path);
    try {
      final bytes = await file.readAsBytes();
      return RecordedAudio('voice-note.m4a', bytes);
    } finally {
      // The recording is only needed for the upload.
      try {
        await file.delete();
      } catch (_) {}
    }
  }

  @override
  Future<void> cancelRecording() async {
    final path = await _recorder?.stop();
    if (path != null) {
      try {
        await File(path).delete();
      } catch (_) {}
    }
  }

  @override
  Future<void> shareFile(String filename, List<int> bytes) async {
    final dir = await getTemporaryDirectory();
    final file = File(
      '${dir.path}${Platform.pathSeparator}${safeUploadName(filename)}',
    );
    await file.writeAsBytes(bytes, flush: true);
    await SharePlus.instance.share(ShareParams(files: [XFile(file.path)]));
  }
}
