//! Real file-type icons for activity steps and file rows.
//!
//! The SVGs are the file-type icons of the MIT-licensed vscode-icons project,
//! vendored under `assets/filetypes/` (see `SOURCES.md` there). They are
//! embedded at compile time and shown as `data:` URIs, so nothing is fetched
//! at run time and no icon is ever invented locally.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

macro_rules! svg {
    ($name:literal) => {
        include_str!(concat!("../assets/filetypes/", $name, ".svg"))
    };
}

/// The icon group for a file extension (lowercase, no dot), or `None` when we
/// have no real icon for it. The groups are the vendored vscode-icons files.
pub fn file_type_of(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "pdf",
        "doc" | "docx" | "odt" | "rtf" => "word",
        "ppt" | "pptx" | "odp" => "powerpoint",
        "xls" | "xlsx" | "xlsm" | "ods" => "excel",
        "md" | "markdown" => "markdown",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => "image",
        "mp4" | "mov" | "webm" | "mkv" | "avi" => "video",
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => "audio",
        "zip" | "rar" | "7z" | "tar" | "gz" => "zip",
        "json" | "yaml" | "yml" | "toml" | "xml" => "json",
        "csv" | "tsv" => "csv",
        "txt" | "log" => "text",
        _ => return None,
    })
}

/// A `data:` URI for the real file-type icon of `group` (as returned by
/// [`file_type_of`]), or `None` for an unknown group.
pub fn file_type_image_src(group: &str) -> Option<String> {
    let svg = match group {
        "pdf" => svg!("pdf"),
        "word" => svg!("word"),
        "powerpoint" => svg!("powerpoint"),
        "excel" => svg!("excel"),
        "markdown" => svg!("markdown"),
        "image" => svg!("image"),
        "video" => svg!("video"),
        "audio" => svg!("audio"),
        "zip" => svg!("zip"),
        "json" => svg!("json"),
        "csv" => svg!("csv"),
        "text" => svg!("text"),
        _ => return None,
    };
    Some(format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(svg)
    ))
}

/// A `data:` URI for the real file-type icon of one file extension
/// (any case, with or without the leading dot), or `None`.
pub fn file_ext_image_src(ext: &str) -> Option<String> {
    let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
    file_type_image_src(file_type_of(&ext)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_map_to_vendored_icons() {
        for (ext, group) in [
            ("pdf", "pdf"),
            ("DOCX", "word"),
            ("pptx", "powerpoint"),
            ("XLSX", "excel"),
            ("md", "markdown"),
            ("png", "image"),
            ("mp4", "video"),
            ("mp3", "audio"),
            ("zip", "zip"),
            ("json", "json"),
            ("csv", "csv"),
            ("txt", "text"),
        ] {
            let got = file_type_of(&ext.to_ascii_lowercase())
                .unwrap_or_else(|| panic!("missing group for {ext}"));
            assert_eq!(got, group);
            let src = file_ext_image_src(ext).unwrap_or_else(|| panic!("missing icon for {ext}"));
            assert!(src.starts_with("data:image/svg+xml;base64,"), "{ext}");
        }
    }

    #[test]
    fn unknown_extensions_have_no_icon() {
        assert!(file_type_of("exe").is_none());
        assert!(file_ext_image_src(".nope").is_none());
        assert!(file_ext_image_src("").is_none());
    }

    #[test]
    fn every_group_has_a_real_vendored_svg() {
        for group in [
            "pdf",
            "word",
            "powerpoint",
            "excel",
            "markdown",
            "image",
            "video",
            "audio",
            "zip",
            "json",
            "csv",
            "text",
        ] {
            let src = file_type_image_src(group)
                .unwrap_or_else(|| panic!("missing vendored svg for {group}"));
            let bytes = STANDARD.decode(src.trim_start_matches("data:image/svg+xml;base64,"));
            let bytes = bytes.expect("icon must be valid base64");
            let text = std::str::from_utf8(&bytes).expect("icon must be utf-8 svg");
            assert!(text.contains("<svg"), "{group} must be a real svg");
        }
    }
}
