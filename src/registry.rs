//! The shared project list. A stable lock file protects the whole read/modify/rename, so
//! simultaneous processes cannot discard each other's entries. The OS releases it on exit.
use fs2::FileExt;
use serde_json::{json, Value};
use std::io::{Error, ErrorKind};
use std::path::Path;

pub fn update(path: &Path, root: &Path, last_run: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock"))?;
    lock.lock_exclusive()?;
    let mut entries: Vec<Value> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| Error::new(ErrorKind::InvalidData, e))?,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let root = root.display().to_string();
    if let Some(entry) = entries.iter_mut().find(|e| e["root"].as_str() == Some(&root)) {
        entry["lastRun"] = json!(entry["lastRun"].as_u64().unwrap_or(0).max(last_run));
    } else {
        entries.push(json!({"root": root, "lastRun": last_run}));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry["lastRun"].as_u64().unwrap_or(0)));
    // write_output replaces through a temporary file and rename. Readers need no lock.
    crate::cli::write_output(path, (serde_json::to_string_pretty(&entries)? + "\n").as_bytes(), true).map(|_| ()).map_err(|e| Error::other(e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_updates_keep_every_project_and_latest_time() {
        let dir = crate::auth::tests::temp_dir("registry");
        let path = dir.join("state/projects.json");
        let workers: Vec<_> = (0..12)
            .map(|i| {
                let (path, root) = (path.clone(), dir.join(format!("game-{i}")));
                std::thread::spawn(move || update(&path, &root, i).unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        update(&path, &dir.join("game-0"), 20).unwrap();
        update(&path, &dir.join("game-0"), 1).unwrap();
        let entries: Vec<Value> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entries.len(), 12);
        assert_eq!(entries[0], json!({"root": dir.join("game-0").display().to_string(), "lastRun": 20}));
    }

    #[test]
    fn a_broken_registry_is_preserved() {
        let dir = crate::auth::tests::temp_dir("registry-broken");
        let path = dir.join("projects.json");
        std::fs::write(&path, "my broken file").unwrap();
        assert!(update(&path, &dir, 1).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "my broken file");
    }

    #[test]
    fn registry_worker() {
        // Subprocesses use isolated paths; no test changes this process's environment.
        if let (Ok(path), Ok(root)) = (std::env::var("CODEX_IMG_TEST_REGISTRY"), std::env::var("CODEX_IMG_TEST_PROJECT")) {
            update(Path::new(&path), Path::new(&root), 123).unwrap();
        }
    }

    #[test]
    fn concurrent_processes_do_not_lose_projects() {
        let dir = crate::auth::tests::temp_dir("registry-processes");
        let path = dir.join("projects.json");
        let mut workers: Vec<_> = (0..8)
            .map(|i| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["registry::tests::registry_worker", "--exact"])
                    .env("CODEX_IMG_TEST_REGISTRY", &path)
                    .env("CODEX_IMG_TEST_PROJECT", dir.join(format!("game-{i}")))
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for worker in &mut workers {
            assert!(worker.wait().unwrap().success());
        }
        let entries: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(entries.len(), 8);
    }
}
