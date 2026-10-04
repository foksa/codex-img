use crate::error::{Error, Result};
use std::{path::{Path, PathBuf}, io::Write};
fn create_parent(path: &Path) -> Result<()> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => std::fs::create_dir_all(parent).map_err(|e| Error::other(format!("Could not create {}: {e}", parent.display()))),
        None => Ok(()),
    }
}

/// Write `bytes` to a new file at `path`, creating its directory. With `overwrite`, replace an
/// existing file instead; returns false when it already held exactly these bytes.
pub fn write_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<bool> {
    create_parent(path)?;
    if overwrite {
        write_replacing(path, bytes)
    } else {
        write_new(path, bytes).map(|()| true)
    }
}

/// Replace `path` with `bytes` through a temporary file and a rename, so nothing ever sees half a
/// file. Returns false without writing when the file already holds exactly these bytes: output is
/// deterministic, so re-running a pipeline leaves untouched files alone (git, CDN uploads).
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let temp = temp_path(path);
    write_file(&temp, bytes)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::other(format!("Could not replace {}: {e}", path.display()))
    })?;
    Ok(true)
}

/// Write `bytes` to `path`, which must not exist yet. The file only appears under its name once
/// it's complete: a half-written raw image would look already generated to `batch`. So the bytes go
/// to a temporary file first, which is then hard-linked into place; unlike a rename, a link never
/// replaces an existing file.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = temp_path(path);
    write_file(&temp, bytes)?;
    let linked = std::fs::hard_link(&temp, path);
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::other(format!("Could not create {}: {e}", path.display()))),
        // A filesystem without hard links (FAT, some network shares): create the file directly.
        // write_file still removes it if the write fails.
        Err(_) => write_file(path, bytes),
    }
}

/// A hidden temporary name next to `path`, on the same filesystem.
fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", unique_id()))
}

/// Create `path` (never replacing a file) and write `bytes` to it, removing it again if the write
/// fails, for example on a full disk.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::other(format!("Could not create {}: {e}", path.display())))?;
    if let Err(e) = file.write_all(bytes) {
        // Closed first: Windows can't remove an open file.
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Error::other(format!("Could not write {}: {e}", path.display())));
    }
    Ok(())
}


fn unique_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_files_appear_whole_and_never_replace_one() {
        let dir = tempfile::tempdir().unwrap().keep();
        write_new(&dir.join("a.png"), b"first").unwrap();
        let err = write_new(&dir.join("a.png"), b"second").unwrap_err();
        assert!(err.message.contains("Could not create"), "{}", err.message);
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"first");
        assert!(write_output(&dir.join("a.png"), b"third", true).unwrap());
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"third");
        // No temporary files are left behind, whichever way it went.
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.png"]);
    }

}
