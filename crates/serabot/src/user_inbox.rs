//! Paths of files in the server inbox, scoped to one user.
//!
//! The inbox is one directory on disk, but every user's files live under
//! `<inbox>/<user_id>/`: Telegram uploads and `local_import` both write there,
//! and the path a tool hands back starts with `<user_id>/`. A tool that takes an
//! inbox path calls [`resolve`], so a path must name a file inside the acting
//! user's own directory. The containment check runs on canonical paths, so a
//! symlink or junction that points out of that directory is refused as well.

use std::path::{Path, PathBuf};

use crate::error::{Result, SeraError};

/// Longest inbox path a tool accepts.
const MAX_INBOX_PATH: usize = 512;

/// Whether `user_id` is a safe directory name under the inbox: ASCII letters,
/// digits, `-` and `_`, at most 128 characters.
pub fn safe_user_segment(user_id: &str) -> bool {
    !user_id.is_empty()
        && user_id.len() <= 128
        && user_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Resolve `value`, an inbox path such as `<user_id>/local/report.pdf`, to an
/// existing path inside `<inbox>/<user_id>/`.
///
/// * A path that starts with another user's id, or that points outside the
///   user's directory after canonicalisation, is reported as not found. The
///   reply never reveals whether the other user's file exists.
/// * Absolute paths, drive prefixes, empty, `.` and `..` components are
///   refused before the filesystem is touched.
///
/// The returned path is canonical and may be a file or a folder; callers check
/// the kind they need.
pub fn resolve(inbox: &Path, user_id: &str, value: &str) -> Result<PathBuf> {
    if !safe_user_segment(user_id) {
        return Err(SeraError::Forbidden(
            "the account id is not a safe inbox directory name".into(),
        ));
    }
    if value.is_empty() || value.len() > MAX_INBOX_PATH || value.contains(['\0', ':']) {
        return Err(SeraError::Validation("invalid inbox path".into()));
    }
    if value.starts_with(['/', '\\']) {
        return Err(SeraError::Forbidden(
            "inbox paths must be relative to your inbox".into(),
        ));
    }
    let parts: Vec<&str> = value.split(['/', '\\']).collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(SeraError::Forbidden(
            "inbox paths must be relative to your inbox and cannot contain '..'".into(),
        ));
    }
    let not_found = || SeraError::NotFound("file not found in your inbox".into());
    if parts[0] != user_id {
        return Err(not_found());
    }
    let root = inbox
        .canonicalize()
        .map_err(|_| SeraError::NotFound("the inbox is unavailable".into()))?;
    // The user's own directory must be a real directory, not a link (symlink
    // or Windows junction) that points elsewhere, even inside the inbox.
    let user_link = root.join(user_id);
    let link = std::fs::symlink_metadata(&user_link).map_err(|_| not_found())?;
    if is_link(&link) {
        return Err(not_found());
    }
    let user_dir = user_link.canonicalize().map_err(|_| not_found())?;
    if user_dir != user_link || !user_dir.starts_with(&root) {
        return Err(not_found());
    }
    let mut candidate = user_dir.clone();
    for part in &parts[1..] {
        candidate.push(part);
    }
    let candidate = candidate.canonicalize().map_err(|_| not_found())?;
    if !candidate.starts_with(&user_dir) {
        return Err(not_found());
    }
    Ok(candidate)
}

/// Whether `metadata` (taken without following links) is a symlink, a junction
/// or another reparse point.
fn is_link(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox_with_two_users() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(inbox.join("alice").join("local")).unwrap();
        std::fs::create_dir_all(inbox.join("bob")).unwrap();
        std::fs::write(
            inbox.join("alice").join("local").join("secret.pdf"),
            b"%PDF",
        )
        .unwrap();
        std::fs::write(inbox.join("bob").join("mine.txt"), b"bob").unwrap();
        (dir, inbox)
    }

    #[test]
    fn a_user_reaches_their_own_files_in_either_separator() {
        let (_dir, inbox) = inbox_with_two_users();
        let file = resolve(&inbox, "alice", "alice/local/secret.pdf").unwrap();
        assert!(file.ends_with("secret.pdf") && file.is_file());
        assert!(resolve(&inbox, "alice", "alice\\local\\secret.pdf").is_ok());
        assert!(resolve(&inbox, "alice", "alice/local").is_ok());
    }

    #[test]
    fn another_users_file_is_not_found() {
        let (_dir, inbox) = inbox_with_two_users();
        // Bob names Alice's file, by her prefix and by a traversal back to it.
        for path in [
            "alice/local/secret.pdf",
            "alice\\local\\secret.pdf",
            "bob/../alice/local/secret.pdf",
        ] {
            let error = resolve(&inbox, "bob", path).unwrap_err();
            assert!(
                matches!(error, SeraError::NotFound(_) | SeraError::Forbidden(_)),
                "{path}: {error}"
            );
        }
        // The prefix mismatch reads as "not found", never as a permission hint.
        assert!(matches!(
            resolve(&inbox, "bob", "alice/local/secret.pdf").unwrap_err(),
            SeraError::NotFound(_)
        ));
        assert!(resolve(&inbox, "bob", "mine.txt").is_err());
    }

    #[test]
    fn traversal_absolute_and_drive_paths_are_refused() {
        let (_dir, inbox) = inbox_with_two_users();
        for path in [
            "",
            "/alice/local/secret.pdf",
            "\\\\server\\share",
            "C:\\Windows\\win.ini",
            "alice/../bob/mine.txt",
            "alice/./local/secret.pdf",
            "alice//local/secret.pdf",
            "alice/local/secret.pdf\0",
        ] {
            assert!(resolve(&inbox, "alice", path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn links_that_leave_the_user_directory_are_not_followed() {
        let (_dir, inbox) = inbox_with_two_users();
        let link = inbox.join("bob").join("shortcut.pdf");
        let target = inbox.join("alice").join("local").join("secret.pdf");
        if !create_file_link(&target, &link) {
            // Links are not available on this host; nothing to check.
            return;
        }
        let error = resolve(&inbox, "bob", "bob/shortcut.pdf").unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)), "{error}");
    }

    #[cfg(unix)]
    fn create_file_link(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    #[cfg(windows)]
    fn create_file_link(target: &Path, link: &Path) -> bool {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }

    /// A folder link: a symlink where the host allows it, else a junction.
    fn create_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|out| out.status.success());
        linked
    }

    #[test]
    fn a_user_directory_that_is_a_link_is_refused() {
        let (_dir, inbox) = inbox_with_two_users();
        // Carol's directory is a link to Alice's: the path looks like Carol's,
        // but the files are Alice's.
        if !create_dir_link(&inbox.join("alice"), &inbox.join("carol")) {
            // Links are not available on this host; nothing to check.
            return;
        }
        let error = resolve(&inbox, "carol", "carol/local/secret.pdf").unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)), "{error}");
        // A real directory of the same shape still resolves.
        assert!(resolve(&inbox, "alice", "alice/local/secret.pdf").is_ok());
    }

    #[test]
    fn unsafe_account_ids_are_refused() {
        let (_dir, inbox) = inbox_with_two_users();
        for user in ["", "../alice", "a b", "a/b"] {
            assert!(
                resolve(&inbox, user, "alice/local/secret.pdf").is_err(),
                "{user:?}"
            );
        }
    }
}
