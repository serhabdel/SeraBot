fn main() {
    // Build-time inputs read with option_env! in src/updater.rs.
    // Absent values disable updates (developer build) without any network call.
    println!("cargo:rerun-if-env-changed=SERABOT_RELEASES_REPO");
    println!("cargo:rerun-if-env-changed=SERABOT_BUILD_SHA");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/serabot.ico");
    println!("cargo:rerun-if-changed=../serabot/assets/bots");
    generate_default_bot_avatars();
    #[cfg(windows)]
    embed_windows_metadata();
}

/// Embed every shipped bot PNG (`../serabot/assets/bots/*.png`) at compile time
/// so bot avatars render immediately and never disappear on refresh/reconnect.
fn generate_default_bot_avatars() {
    use std::fmt::Write as _;
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"),
    );
    let bots_dir = manifest_dir.join("../serabot/assets/bots");
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&bots_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let is_png = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
            if !is_png {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) {
                println!("cargo:rerun-if-changed={}", path.display());
                let abs = std::fs::canonicalize(&path).unwrap_or(path);
                entries.push((stem, abs));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::from("pub const DEFAULT_BOT_AVATARS: &[(&str, &[u8])] = &[\n");
    for (slug, path) in entries {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let clean = normalized.strip_prefix("//?/").unwrap_or(&normalized);
        let _ = writeln!(out, "    ({slug:?}, include_bytes!({clean:?})),");
    }
    out.push_str("];\n");

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    std::fs::write(out_dir.join("default_bot_avatars.rs"), out)
        .expect("write default_bot_avatars.rs");
}

/// Version metadata and a per-monitor-v2 DPI manifest for the Windows exe.
/// The exe icon is embedded here; the window icon is set in `ui.rs`.
#[cfg(windows)]
fn embed_windows_metadata() {
    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", "SeraBot Owner")
        .set("FileDescription", "SeraBot owner console")
        .set("ProductVersion", env!("CARGO_PKG_VERSION"))
        .set("FileVersion", env!("CARGO_PKG_VERSION"))
        .set("OriginalFilename", "serabot-desktop.exe")
        .set("LegalCopyright", "SeraBot")
        .set_icon("assets/serabot.ico")
        .set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.0.0.0" processorArchitecture="*" name="SeraBot.Owner" type="win32"/>
  <description>SeraBot Owner</description>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">permonitorv2,permonitor</dpiAwareness>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#,
        );
    if let Err(error) = resource.compile() {
        // Keep the exe buildable on hosts without the Windows resource tools.
        println!("cargo:warning=Windows resource metadata was not embedded: {error}");
    }
}
