package com.serabot.serabot_mobile

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                if (call.method != "install") {
                    result.notImplemented()
                    return@setMethodCallHandler
                }
                val path = call.argument<String>("path")
                if (path == null) {
                    result.error("bad_args", "path required", null)
                    return@setMethodCallHandler
                }
                try {
                    result.success(installApk(path))
                } catch (e: Exception) {
                    result.error("install_failed", e.javaClass.simpleName, null)
                }
            }
    }

    private fun installApk(path: String): String {
        // Only APKs inside the app's own updates cache directory are installable.
        val updatesDir = File(cacheDir, "updates").canonicalFile
        val file = File(path).canonicalFile
        if (file.parentFile != updatesDir || !file.name.endsWith(".apk") || !file.isFile) {
            throw IllegalArgumentException("not an update file")
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !packageManager.canRequestPackageInstalls()) {
            // "Allow from this source": send the user to the per-app toggle.
            startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            )
            return "needs_permission"
        }
        val uri = FileProvider.getUriForFile(this, "$packageName.updates", file)
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        startActivity(intent)
        return "launched"
    }

    companion object {
        private const val CHANNEL = "tech.seradyx.serabot/installer"
    }
}
