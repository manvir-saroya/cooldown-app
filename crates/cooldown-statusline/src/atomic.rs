//! Atomic file replacement.
//!
//! Claude Code cancels an in-flight statusline run when a newer update arrives,
//! and the Cooldown app may read the inbox at any moment, so a file must never
//! be observable half-written. Every write goes to a temp file in the same
//! directory and is then renamed over the target (an atomic replace on both
//! POSIX and Windows NTFS).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The temp-file name for `target`: `.<name>.<pid>.<counter>.<nanos>.tmp` in the
/// same directory, so the rename never crosses a filesystem boundary.
pub fn temp_path_for(target: &Path) -> PathBuf {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{name}.{}.{n}.{nanos}.tmp", std::process::id()))
}

/// Atomically replace `target` with `bytes`.
///
/// If `target` is a symlink (dotfile managers do this to `settings.json`), the
/// link is followed so it stays a link. Permissions of an existing target are
/// copied onto the replacement. The temp file is removed if anything fails.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = resolve_symlink(target);
    let tmp = temp_path_for(&target);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        if let Ok(meta) = fs::metadata(&target) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        rename_with_retry(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Follow a symlink so the link itself survives a rename-over.
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// On Windows a reader that has the target open without `FILE_SHARE_DELETE`
/// makes the rename fail briefly, so retry a few times. Elsewhere one try is
/// enough because errors are not transient.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let attempts: u32 = if cfg!(windows) { 5 } else { 1 };
    let mut last = None;
    for attempt in 0..attempts {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last = Some(err);
                if attempt + 1 < attempts {
                    thread::sleep(Duration::from_millis(4 * (u64::from(attempt) + 1)));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Delete orphaned temp files for `final_name` that are older than `max_age`.
///
/// A run cancelled between "create temp" and "rename" leaves its temp file
/// behind, so each successful write sweeps old ones. Only files that match our
/// exact `.<final_name>.*.tmp` pattern are touched. Returns how many were
/// removed.
pub fn cleanup_stale_temps(dir: &Path, final_name: &str, max_age: Duration) -> usize {
    let prefix = format!(".{final_name}.");
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(&prefix) && name.ends_with(".tmp")) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if old_enough && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn writes_new_file_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.json");
        write_atomic(&target, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"{\"a\":1}");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "only the target should exist: {names:?}");
    }

    #[test]
    fn replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.json");
        fs::write(&target, "old old old old old").unwrap();
        write_atomic(&target, b"new").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn failure_cleans_up_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // The target is an existing directory, so the final rename fails.
        let target = dir.path().join("a-dir");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("child"), "x").unwrap();
        assert!(write_atomic(&target, b"data").is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp left behind: {leftovers:?}");
    }

    #[test]
    fn readers_never_see_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("inbox.json");
        let payloads: Vec<String> = (0..4)
            .map(|i| format!("{{\"writer\":{i},\"pad\":\"{}\"}}", "x".repeat(200_000)))
            .collect();
        write_atomic(&target, payloads[0].as_bytes()).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let target = target.clone();
            let stop = Arc::clone(&stop);
            let payloads = payloads.clone();
            thread::spawn(move || {
                let mut reads = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(text) = fs::read_to_string(&target) {
                        assert!(
                            payloads.contains(&text),
                            "observed a torn file of {} bytes",
                            text.len()
                        );
                        reads += 1;
                    }
                }
                reads
            })
        };
        let writers: Vec<_> = payloads
            .iter()
            .map(|p| {
                let target = target.clone();
                let p = p.clone();
                thread::spawn(move || {
                    for _ in 0..40 {
                        write_atomic(&target, p.as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_so_the_link_survives() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permissions_of_existing_target() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("s.json");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&target, b"new").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn cleanup_removes_only_old_matching_temps() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(".claude-statusline.json.1.0.0.tmp");
        let fresh = dir.path().join(".claude-statusline.json.2.0.0.tmp");
        let other = dir.path().join(".something-else.json.1.0.0.tmp");
        let real = dir.path().join("claude-statusline.json");
        for p in [&old, &fresh, &other, &real] {
            fs::write(p, "x").unwrap();
        }
        let long_ago = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        let removed = cleanup_stale_temps(
            dir.path(),
            "claude-statusline.json",
            Duration::from_secs(60),
        );
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists(), "fresh temp may belong to a live writer");
        assert!(other.exists(), "foreign files are never touched");
        assert!(real.exists());
    }
}
