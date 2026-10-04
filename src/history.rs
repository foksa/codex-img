//! Stage replacements before touching the active raw; a backend failure must not empty an asset.
use crate::{batch, cli, error::{Error, Result}, manifest, project, util};
use serde_json::Value;
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug)]
pub struct Replacement {
    pub raw: PathBuf,
    pub previous: Option<PathBuf>,
    old_image: Option<Vec<u8>>,
    old_note: Option<Vec<u8>>,
    staging: PathBuf,
}
impl Replacement {
    pub fn plan(raw: &Path, spec: &Path, key: &str) -> Result<Self> {
        let raw = batch::resolved(raw);
        let folder = folder(spec, key);
        let name = format!("{}_{}", util::stamp(util::now_secs()), util::random_id());
        let read = |path: &Path| match fs::read(path) { Ok(bytes) => Ok(Some(bytes)), Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None), Err(e) => Err(Error::other(e.to_string())) };
        let old_image = read(&raw)?;
        let old_note = read(&manifest::path_for(&raw))?;
        if old_image.is_none() && old_note.is_some() { return Err(Error::usage("The raw is missing but its manifest exists. Preserve or move that manifest before replacing the asset.")); }
        Ok(Self {raw, previous: old_image.as_ref().map(|_| folder.join(format!("{name}.png"))), old_image, old_note, staging: folder.join(format!("{name}.pending.png"))})
    }
    pub fn commit(&self, bytes: &[u8], note: &Value) -> Result<()> {
        let raw_note = manifest::path_for(&self.raw);
        cli::write_output(&self.staging, bytes, false)?;
        let staged_note = manifest::path_for(&self.staging);
        manifest::write(&staged_note, note)?;
        if fs::read(&self.raw).ok() != self.old_image || fs::read(&raw_note).ok() != self.old_note {
            return Err(Error::other(format!("The current raw or manifest changed during the run. The current asset was preserved; the new version is kept at {}.", self.staging.display())));
        }
        if let Some(previous) = &self.previous {
            move_file(&self.raw, previous).map_err(|e| Error::other(e.to_string()))?;
            if self.old_note.is_some() {
                if let Err(error) = move_file(&raw_note, &manifest::path_for(previous)) {
                    let _ = move_file(previous, &self.raw);
                    return Err(Error::other(error.to_string()));
                }
            }
        }
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(self.raw.parent().unwrap())?;
            // Both writes refuse existing paths, including when raw_dir is on another volume.
            cli::write_output(&self.raw, bytes, false).map_err(|error| std::io::Error::other(error.message))?;
            if let Err(error) = manifest::write(&raw_note, note) { let _ = fs::remove_file(&self.raw); return Err(std::io::Error::other(error.message)); }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(previous) = &self.previous {
                if !self.raw.exists() { let _ = move_file(previous, &self.raw); }
                if self.old_note.is_some() && !raw_note.exists() { let _ = move_file(&manifest::path_for(previous), &raw_note); }
            }
            return Err(Error::other(format!("Could not activate the replacement: {error}. The generated version is kept at {}.", self.staging.display())));
        }
        let _ = fs::remove_file(&self.staging); let _ = fs::remove_file(staged_note);
        Ok(())
    }
}

pub fn folder(spec: &Path, key: &str) -> PathBuf {
    let base = project::root(spec.parent().unwrap_or(Path::new("."))).unwrap_or_else(|| batch::resolved(spec.parent().unwrap_or(Path::new("."))));
    base.join(".codex-img/history").join(key)
}

pub fn lock(spec: &Path, no_wait: bool) -> Result<fs::File> {
    lock_with_notice(spec, no_wait, &mut |message| eprintln!("{message}"))
}
fn lock_with_notice(spec: &Path, no_wait: bool, notice: &mut dyn FnMut(String)) -> Result<fs::File> {
    use fs2::FileExt;
    let canonical = spec.canonicalize().map_err(|e| Error::other(e.to_string()))?;
    let base = project::root(canonical.parent().unwrap()).unwrap_or_else(|| canonical.parent().unwrap().to_path_buf());
    let folder = base.join(".codex-img/locks"); fs::create_dir_all(&folder).map_err(|e| Error::other(e.to_string()))?;
    let hash = util::fnv1a64(canonical.as_os_str().as_encoded_bytes());
    let file = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(folder.join(format!("{hash:016x}.lock"))).map_err(|e| Error::other(e.to_string()))?;
    if let Err(error) = file.try_lock_exclusive() {
        // Windows reports a held lock as a lock violation, not WouldBlock.
        let contended = error.kind() == std::io::ErrorKind::WouldBlock || error.raw_os_error() == fs2::lock_contended_error().raw_os_error();
        if !contended { return Err(Error::other(error.to_string())); }
        if no_wait { return Err(Error::usage(format!("Another batch run holds the lock on {} (--no-wait).", canonical.display()))); }
        notice(format!("Waiting for another batch run on {}…", canonical.display()));
        file.lock_exclusive().map_err(|e| Error::other(e.to_string()))?;
    }
    remove_unused_legacy_lock(&base);
    Ok(file)
}

fn remove_unused_legacy_lock(base: &Path) {
    use fs2::FileExt;
    let path = base.join(".codex-img/batch.lock");
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) { return; }
    // Never remove a file held by an older CLI. Cleanup is optional and must not block work.
    if let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) {
        if file.try_lock_exclusive().is_ok() { let _ = fs::remove_file(path); }
    }
}

// Link/copy followed by unlink also works across volumes and refuses occupied history names.
fn move_file(source: &Path, target: &Path) -> std::io::Result<()> {
    if let Err(error) = fs::hard_link(source, target) {
        if error.kind() == std::io::ErrorKind::AlreadyExists { return Err(error); }
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new().create_new(true).write(true).open(target)?;
        if let Err(error) = std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all()) {
            drop(output); let _ = fs::remove_file(target); return Err(error);
        }
        output.set_permissions(fs::metadata(source)?.permissions())?;
    }
    fs::remove_file(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn obsolete_batch_lock_is_removed_only_when_unused() {
        use fs2::FileExt;
        let dir = crate::auth::tests::temp_dir("legacy-batch-lock");
        fs::create_dir_all(dir.join(".codex-img")).unwrap();
        let spec = dir.join("assets.json"); fs::write(&spec, "{}").unwrap();
        let legacy = dir.join(".codex-img/batch.lock"); fs::write(&legacy, b"legacy lock").unwrap();
        let held = fs::OpenOptions::new().read(true).write(true).open(&legacy).unwrap();
        held.lock_exclusive().unwrap();
        drop(lock(&spec, true).unwrap());
        drop(held); // Windows locks are mandatory: the file can only be read once released.
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy lock");
        let current = lock(&spec, true).unwrap();
        assert!(!legacy.exists());
        assert!(lock(&spec, true).is_err(), "modern spec lock stays held");
        drop(current);
    }
    #[test]
    fn locks_are_per_canonical_spec_and_waiting_is_reported_or_refused() {
        let dir = crate::auth::tests::temp_dir("spec-locks"); std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let first = dir.join("first.json"); let second = dir.join("second.json");
        fs::write(&first, "{}").unwrap(); fs::write(&second, "{}").unwrap();
        let held = lock(&first, true).unwrap(); let other = lock(&second, true).unwrap(); drop(other);
        assert!(lock(&dir.join("./first.json"), true).unwrap_err().message.contains("--no-wait"));
        let (send, receive) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || lock_with_notice(&first, false, &mut |message| send.send(message).unwrap()).unwrap());
        let notice = receive.recv_timeout(std::time::Duration::from_secs(5)).unwrap(); assert!(notice.contains("Waiting for another batch run on") && notice.contains("first.json"));
        assert!(!handle.is_finished()); drop(held); drop(handle.join().unwrap());
    }
    #[test]
    fn preserves_concurrent_changes_and_keeps_the_generated_replacement() {
        let dir = crate::auth::tests::temp_dir("history-race"); let spec = dir.join("assets.json");
        let raw = dir.join("raw/hero.png");
        cli::write_output(&raw, b"original", false).unwrap(); manifest::write(&manifest::path_for(&raw), &json!({"comment":"Old"})).unwrap();
        let replacement = Replacement::plan(&raw, &spec, "actors/hero").unwrap();
        crate::review::edit(&manifest::path_for(&raw), None, "comment", &json!("New note"), None).unwrap();
        assert!(replacement.commit(b"generated", &json!({"prompt":"New"})).unwrap_err().message.contains("preserved"));
        assert_eq!(fs::read(&raw).unwrap(), b"original"); assert_eq!(manifest::read(&manifest::path_for(&raw)).unwrap()["comment"], "New note");
        assert_eq!(fs::read(&replacement.staging).unwrap(), b"generated"); assert!(manifest::path_for(&replacement.staging).is_file());
    }
}
