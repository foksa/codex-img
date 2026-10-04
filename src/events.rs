//! An append-only log of generation events, one JSON object per line, for other programs to follow
//! while images are being made. Writing it never fails a generation: errors are ignored.
//!
//! A run is one command (a plain run, `batch` or `tile`); its jobs are the images it generates.
//! Each line has `v` (format version), `t` (Unix milliseconds), `event`, `run` (the run's id),
//! `job` (on job events, an id shared by one image's events) and `pid`, plus the event's fields:
//! - `run.started`: `source`, `generation` (backend work or free local work), `cwd`, `jobs`,
//!   and for `batch` the `spec` path
//! - `run.ended`: `code` (the exit code it maps to), `done`, `failed`, `durationMs`, and `message`
//!   when the run stopped on an error of its own (a login error before any job, say)
//! - `job.started`: `prompt` (as given), `submittedPrompt` (as sent, when presets or labels changed
//!   it), `inputs` [{path, role}], `request` {aspect, size, quality, background: those set}, and
//!   when known `output` (as asked for), `key` (batch) and `index` (`-n`, from 0)
//! - `job.stage`: `stage` as the backend reports it (`generating`, `in_progress`, ...)
//! - `job.done`: `path`, `durationMs`, and when known `rawPath`, `manifestPath`, `size`,
//!   `historyPath` (the previous batch raw's archive)
//! - `job.failed`: `code` and `message`
//!
//! Every job started ends with `job.done` or `job.failed`, and every run with `run.ended`, unless
//! the process is killed: a reader can tell by `pid` that it's gone.
//! Version 2 paths in project run files are relative to the project root; outside paths and
//! global-log paths are absolute. Version 1 paths remain absolute. Each project run has its own
//! `.codex-img/runs/<UTC stamp>_<id>.ndjson`, with no rotation. The global log is `$CODEX_IMG_EVENTS`, else `$XDG_STATE_HOME/codex-img/events.ndjson`,
//! else `~/.local/state/codex-img/events.ndjson` (`%LOCALAPPDATA%` on Windows). `CODEX_IMG_EVENTS=off`
//! turns it off. Past `MAX_BYTES` the log moves to `events.ndjson.1` and a new one starts.
//! Without `CODEX_IMG_EVENTS`, a top-level `"events": false` in the project's `codex-img.json`, else
//! in the global `presets.json`, turns logging off the same way.

use crate::error::Error;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: u32 = 2;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
static WRITE: Mutex<()> = Mutex::new(());

/// One command's generations. Clones share the run, so jobs on other threads count towards it.
#[derive(Clone)]
pub struct Run(Arc<RunState>);

struct RunState {
    id: String,
    log: Option<PathBuf>,
    root: Option<PathBuf>,
    project_log: bool,
    t: u64,
    started: Instant,
    done: AtomicUsize,
    failed: AtomicUsize,
    worst: AtomicI32,
}

impl Run {
    /// Start a run of `jobs` planned images and write `run.started`, with `fields` added.
    pub fn start(source: &'static str, jobs: usize, fields: Value) -> Run {
        let cwd = std::env::current_dir().unwrap_or_default();
        Self::start_from(&cwd, source, jobs, fields)
    }

    pub fn start_from(cwd: &Path, source: &'static str, jobs: usize, fields: Value) -> Run {
        let (explicit, global) = log_paths();
        let config = crate::presets::global_dir().map(|dir| dir.join(crate::presets::GLOBAL_FILE));
        Self::start_at(cwd, explicit, global, config.as_deref(), source, jobs, fields)
    }

    fn start_at(cwd: &Path, explicit: Option<PathBuf>, global: Option<PathBuf>, config: Option<&Path>, source: &'static str, jobs: usize, fields: Value) -> Run {
        let id = crate::util::random_id();
        let root = crate::project::root(cwd);
        let project_log = explicit.is_none() && root.is_some();
        let setting = || {
            let project = root.as_ref().and_then(|root| crate::presets::events_setting(&root.join(crate::presets::PROJECT_FILE)));
            project.or_else(|| config.and_then(crate::presets::events_setting)).unwrap_or(true)
        };
        let log = match explicit {
            Some(path) => resolve_path(Some(path), None, None, None),
            None if !setting() => None,
            None => root
                .as_ref()
                .map(|root| {
                    let _ = crate::project::prepare(root);
                    let stamp = crate::util::iso8601(crate::util::now_secs()).trim_end_matches('Z').replace(':', "-");
                    root.join(".codex-img/runs").join(format!("{stamp}_{id}.ndjson"))
                })
                .or_else(|| global.clone()),
        };
        let t = now_ms();
        if log.is_some() {
            if let (Some(root), Some(global)) = (&root, &global) {
                let _ = crate::registry::update(&global.with_file_name("projects.json"), root, t);
            }
        }
        let state = RunState { id, log, root, project_log, t, started: Instant::now(), done: AtomicUsize::new(0), failed: AtomicUsize::new(0), worst: AtomicI32::new(0) };
        let run = Run(Arc::new(state));
        let mut head = json!({"source": source, "cwd": absolute(cwd), "jobs": jobs,
            "generation": matches!(source, "run" | "batch" | "tile")});
        if let (Value::Object(head), Value::Object(fields)) = (&mut head, fields) {
            head.extend(fields);
        }
        if !project_log {
            head["root"] = json!(run.0.root.as_ref().map(|r| r.display().to_string()));
        }
        run.emit("run.started", None, head);
        run
    }

    #[cfg(test)]
    fn start_in(log: Option<PathBuf>, source: &'static str, jobs: usize, fields: Value) -> Run {
        let cwd = crate::auth::tests::temp_dir("loose-events");
        Self::start_at(&cwd, log.or_else(|| Some("off".into())), None, None, source, jobs, fields)
    }


    /// An output outside the project cannot discover its path base by walking upward.
    pub fn manifest_root(&self, image: &Path) -> Option<String> {
        self.0.root.as_ref().filter(|root| !crate::batch::resolved(image).starts_with(root))
            .map(|root| root.display().to_string())
    }

    /// Manifests in projects use the same root as events, even when logging is disabled.
    pub fn in_project(&self) -> bool {
        self.0.root.is_some()
    }

    pub fn manifest_path(&self, path: &Path) -> String {
        match &self.0.root {
            Some(root) => crate::project::shown(path, Some(root)),
            None => path.display().to_string(),
        }
    }

    /// Start one image of the run and write `job.started`.
    pub fn job(&self, fields: Value) -> Job {
        let job = Job { run: self.clone(), id: crate::util::random_id() };
        self.emit("job.started", Some(&job.id), fields);
        job
    }

    /// Write `run.ended`. Its code is the worst of the jobs' and `error`'s, the run's own error if
    /// it stopped before or between jobs.
    pub fn end(&self, error: Option<&Error>) {
        let state = &self.0;
        let code = state.worst.load(Ordering::SeqCst).max(error.map_or(0, |e| e.kind.exit_code()));
        self.emit(
            "run.ended",
            None,
            json!({
                "code": code,
                "done": state.done.load(Ordering::SeqCst),
                "failed": state.failed.load(Ordering::SeqCst),
                "durationMs": state.started.elapsed().as_millis() as u64,
                "message": error.map(|e| &e.message),
            }),
        );
    }

    fn emit(&self, event: &str, job: Option<&str>, fields: Value) {
        let Some(log) = &self.0.log else { return };
        let mut line = Map::new();
        line.insert("v".into(), json!(VERSION));
        line.insert("t".into(), json!(if event == "run.started" { self.0.t } else { now_ms() }));
        line.insert("event".into(), json!(event));
        line.insert("run".into(), json!(self.0.id));
        if let Some(job) = job {
            line.insert("job".into(), json!(job));
        }
        line.insert("pid".into(), json!(std::process::id()));
        if let Value::Object(fields) = fields {
            line.extend(fields.into_iter().filter(|(_, v)| !v.is_null()));
        }
        if self.0.project_log {
            if let Some(root) = &self.0.root {
                for key in ["cwd", "spec", "output", "path", "rawPath", "manifestPath", "parent", "historyPath"] {
                    if let Some(Value::String(path)) = line.get_mut(key) {
                        *path = crate::project::shown(Path::new(path), Some(root));
                    }
                }
                if let Some(Value::Array(inputs)) = line.get_mut("inputs") {
                    for input in inputs {
                        if let Some(Value::String(path)) = input.get_mut("path") {
                            *path = crate::project::shown(Path::new(path), Some(root));
                        }
                    }
                }
                line.remove("root");
            }
        }
        let _ = append(log, &Value::Object(line), !self.0.project_log);
    }
}

/// One image being generated. Its events share the job id.
pub struct Job {
    run: Run,
    id: String,
}

impl Job {
    pub fn stage(&self, stage: &str) {
        self.run.emit("job.stage", Some(&self.id), json!({"stage": stage}));
    }

    pub fn done(&self, fields: Value) {
        self.run.0.done.fetch_add(1, Ordering::SeqCst);
        self.run.emit("job.done", Some(&self.id), fields);
    }

    pub fn failed(&self, error: &Error) {
        self.run.0.failed.fetch_add(1, Ordering::SeqCst);
        self.run.0.worst.fetch_max(error.kind.exit_code(), Ordering::SeqCst);
        self.run.emit("job.failed", Some(&self.id), json!({"code": error.kind.exit_code(), "message": error.message}));
    }
}

/// `path` as events show it: absolute, with symlinks and `.`/`..` resolved.
pub fn absolute(path: &Path) -> String {
    crate::batch::resolved(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())).display().to_string()
}

/// The images a request sends, for `job.started`.
pub fn inputs(sent: &[crate::manifest::Input]) -> Value {
    sent.iter().map(|input| json!({"path": absolute(&input.path), "role": input.role})).collect()
}

/// Free review commands also need a lock when event writing is disabled.
pub fn global_log_path() -> Option<PathBuf> {
    #[cfg(test)] { Some(std::env::temp_dir().join(format!("codex-img-review-test-locks-{}", std::process::id())).join("events.ndjson")) }
    #[cfg(not(test))] {
        let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        resolve_path(var("CODEX_IMG_EVENTS"), var("XDG_STATE_HOME"), var("HOME"), var("LOCALAPPDATA"))
            .or_else(|| resolve_path(None, var("XDG_STATE_HOME"), var("HOME"), var("LOCALAPPDATA")))
            .map(|path| crate::batch::resolved(&path))
    }
}

/// Unit tests that generate against a mock backend must not write to the user's log.
#[cfg(test)]
fn log_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    (Some("off".into()), None)
}

#[cfg(not(test))]
fn log_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    (var("CODEX_IMG_EVENTS"), resolve_path(None, var("XDG_STATE_HOME"), var("HOME"), var("LOCALAPPDATA")))
}

fn resolve_path(explicit: Option<PathBuf>, state_home: Option<PathBuf>, home: Option<PathBuf>, local_app_data: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        let off = ["off", "0", "false", "no"].iter().any(|word| path.as_os_str().eq_ignore_ascii_case(word));
        return (!off).then_some(path);
    }
    let dir = state_home.or_else(|| home.map(|h| h.join(".local").join("state"))).or(local_app_data)?;
    Some(dir.join("codex-img").join("events.ndjson"))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Append one line in a single write, so lines from parallel jobs and processes don't interleave.
fn append(log: &Path, line: &Value, rotate: bool) -> std::io::Result<()> {
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dir) = log.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    if rotate && std::fs::metadata(log).is_ok_and(|m| m.len() > MAX_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        std::fs::rename(log, rotated)?;
    }
    let mut text = line.to_string();
    text.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(log)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Kind;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("codex-img-events-{name}-{}", crate::util::random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn resolves_the_log_path() {
        let p = PathBuf::from;
        assert_eq!(resolve_path(Some(p("/x/e.ndjson")), Some(p("/s")), None, None), Some(p("/x/e.ndjson")));
        assert_eq!(resolve_path(Some(p("OFF")), Some(p("/s")), None, None), None);
        assert_eq!(resolve_path(None, Some(p("/s")), Some(p("/h")), None), Some(p("/s/codex-img/events.ndjson")));
        assert_eq!(resolve_path(None, None, Some(p("/h")), None), Some(p("/h/.local/state/codex-img/events.ndjson")));
        assert_eq!(resolve_path(None, None, None, Some(p("C:/L"))), Some(p("C:/L/codex-img/events.ndjson")));
        assert_eq!(resolve_path(None, None, None, None), None);
    }

    #[test]
    fn writes_a_run_and_its_jobs_one_line_per_event() {
        let log = temp_dir("run").join("nested").join("events.ndjson");
        let run = Run::start_in(Some(log.clone()), "batch", 2, json!({"spec": "/art/assets.json"}));
        let job = run.job(json!({"prompt": "a cat", "key": "cat", "submittedPrompt": null}));
        job.stage("generating");
        job.done(json!({"path": "/out/cat.png", "durationMs": 1200}));
        let other = std::thread::spawn({
            let run = run.clone();
            move || run.job(json!({"key": "dog"})).failed(&Error::new(Kind::Quota, "Usage limit reached."))
        });
        other.join().unwrap();
        run.end(None);

        let events = lines(&log);
        let names: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(names, ["run.started", "job.started", "job.stage", "job.done", "job.started", "job.failed", "run.ended"]);
        assert!(events.iter().all(|e| e["v"] == VERSION && e["t"].as_u64().unwrap() > 0 && e["pid"] == std::process::id()));
        assert!(events.iter().all(|e| e["run"] == events[0]["run"]));
        assert!(events[0].get("job").is_none() && events[6].get("job").is_none());
        assert_eq!((&events[1]["job"], &events[3]["job"]), (&events[2]["job"], &events[1]["job"]));
        assert_ne!(events[1]["job"], events[4]["job"]);
        let start = &events[0];
        assert_eq!((&start["source"], &start["jobs"], &start["spec"]), (&json!("batch"), &json!(2), &json!("/art/assets.json")));
        assert!(start["cwd"].is_string());
        assert_eq!((&events[1]["prompt"], &events[1]["key"]), (&json!("a cat"), &json!("cat")));
        assert!(events[1].get("submittedPrompt").is_none(), "null fields are left out");
        assert_eq!((&events[2]["stage"], &events[3]["path"]), (&json!("generating"), &json!("/out/cat.png")));
        assert_eq!((&events[5]["code"], &events[5]["message"]), (&json!(3), &json!("Usage limit reached.")));
        let end = &events[6];
        assert_eq!((&end["code"], &end["done"], &end["failed"]), (&json!(3), &json!(1), &json!(1)), "the worst job's code");
        assert!(end["durationMs"].is_u64() && end.get("message").is_none());
    }

    #[test]
    fn a_run_stopped_by_its_own_error_says_why() {
        let log = temp_dir("stopped").join("events.ndjson");
        Run::start_in(Some(log.clone()), "run", 1, json!({})).end(Some(&Error::auth("Login expired.")));
        let end = &lines(&log)[1];
        assert_eq!((&end["code"], &end["done"], &end["failed"], &end["message"]), (&json!(2), &json!(0), &json!(0), &json!("Login expired.")));
    }

    #[test]
    fn distinguishes_generations_from_local_work_including_batch_restore() {
        for (source, fields, generating) in [
            ("run", json!({}), true), ("tile", json!({}), true),
            ("batch", json!({}), true), ("batch", json!({"generation":false}), false),
            ("convert", json!({}), false), ("sheet", json!({}), false),
        ] {
            let log = temp_dir("generation-kind").join("events.ndjson");
            Run::start_in(Some(log.clone()), source, 1, fields).end(None);
            assert_eq!(lines(&log)[0]["generation"], generating);
        }
    }

    #[test]
    fn shows_paths_absolute_and_resolved() {
        let dir = temp_dir("paths").canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("a")).unwrap();
        assert_eq!(absolute(&dir.join("a/../b/./new.png")), dir.join("b/new.png").display().to_string());
        assert!(Path::new(&absolute(Path::new("x.png"))).is_absolute());
    }

    #[test]
    fn rotates_a_full_log() {
        let log = temp_dir("rotate").join("events.ndjson");
        std::fs::write(&log, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        Run::start_in(Some(log.clone()), "run", 1, json!({}));
        assert_eq!(lines(&log).len(), 1);
        assert_eq!(std::fs::metadata(log.with_extension("ndjson.1")).unwrap().len(), MAX_BYTES + 1);
    }

    fn project() -> PathBuf {
        let dir = temp_dir("project").canonicalize().unwrap();
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("art/raw")).unwrap();
        dir
    }

    #[test]
    fn projects_get_separate_portable_run_files_and_a_registry_entry() {
        let root = project();
        let global = temp_dir("global").join("events.ndjson");
        let run = Run::start_at(&root.join("art"), None, Some(global.clone()), None, "batch", 1, json!({"spec": absolute(&root.join("assets.json"))}));
        let external = temp_dir("external").join("ref.png");
        let job = run.job(json!({"prompt": "cat", "request": {}, "inputs": [
            {"path": absolute(&root.join("art/ref.png")), "role": "style"},
            {"path": absolute(&external), "role": "input"}], "output": absolute(&root.join("art/raw/cat.png"))}));
        job.done(json!({"path": absolute(&root.join("art/cat.png")), "rawPath": absolute(&root.join("art/raw/cat.png")),
            "manifestPath": absolute(&root.join("art/cat.png.json")), "historyPath": absolute(&root.join(".codex-img/history/cat/old.png")), "durationMs": 1}));
        run.end(None);
        let other = Run::start_at(&root, None, Some(global.clone()), None, "run", 1, json!({}));
        other.end(None);
        let log = run.0.log.as_ref().unwrap();
        assert_ne!(log, other.0.log.as_ref().unwrap());
        assert!(log.file_name().unwrap().to_str().unwrap().ends_with(&format!("_{}.ndjson", run.0.id)));
        let events = lines(log);
        assert_eq!(events[0]["cwd"], "art");
        assert_eq!(events[0]["spec"], "assets.json");
        assert!(events[0].get("root").is_none());
        assert_eq!(events[1]["inputs"][0]["path"], "art/ref.png");
        assert_eq!(events[1]["inputs"][1]["path"], absolute(&external));
        assert_eq!(events[1]["output"], "art/raw/cat.png");
        assert_eq!(events[2]["path"], "art/cat.png");
        assert_eq!(events[2]["rawPath"], "art/raw/cat.png");
        assert_eq!(events[2]["manifestPath"], "art/cat.png.json");
        assert_eq!(events[2]["historyPath"], ".codex-img/history/cat/old.png");
        assert!(events.iter().all(|e| e["v"] == 2));
        assert!(!std::fs::read_to_string(log).unwrap().contains(root.to_str().unwrap()));
        assert!(!global.exists());
        let registry: Value = serde_json::from_str(&std::fs::read_to_string(global.with_file_name("projects.json")).unwrap()).unwrap();
        assert_eq!(registry.as_array().unwrap().len(), 1);
        assert_eq!(registry[0]["root"], root.display().to_string());
        assert!(registry[0]["lastRun"].as_u64().unwrap() >= events[0]["t"].as_u64().unwrap());
        assert_eq!(lines(other.0.log.as_ref().unwrap())[0]["cwd"], ".");
    }

    #[test]
    fn override_keeps_absolute_paths_and_off_writes_nothing() {
        let root = project();
        let global = temp_dir("overrides").join("events.ndjson");
        for off in ["off", "OFF", "0", "false", "no"] {
            let run = Run::start_at(&root, Some(off.into()), Some(global.clone()), None, "run", 1, json!({}));
            assert!(run.0.log.is_none());
            assert!(run.in_project(), "manifests are independent of event logging");
            run.end(None);
        }
        assert!(!root.join(".codex-img").exists());
        assert!(!global.parent().unwrap().join("projects.json").exists());
        let log = global.with_file_name("forced.ndjson");
        let run = Run::start_at(&root, Some(log.clone()), Some(global.clone()), None, "run", 1, json!({}));
        run.job(json!({"output": absolute(&root.join("new.png"))})).done(json!({"path": absolute(&root.join("new.png"))}));
        run.end(None);
        let events = lines(&log);
        assert_eq!(events[0]["root"], root.display().to_string());
        assert_eq!(events[0]["cwd"], root.display().to_string());
        assert_eq!(events[2]["path"], root.join("new.png").display().to_string());
        assert!(!root.join(".codex-img").exists());
        assert!(global.with_file_name("projects.json").exists());
    }

    #[test]
    fn the_events_setting_follows_env_then_project_then_global() {
        let config = temp_dir("config").join("presets.json");
        let global = temp_dir("setting").join("events.ndjson");
        let registry = global.with_file_name("projects.json");
        let start = |cwd: &Path, explicit: Option<&str>| {
            let run = Run::start_at(cwd, explicit.map(PathBuf::from), Some(global.clone()), Some(&config), "run", 1, json!({}));
            run.end(None);
            run
        };

        let quiet = project();
        std::fs::write(quiet.join("codex-img.json"), r#"{"events": false}"#).unwrap();
        let run = start(&quiet, None);
        assert!(run.0.log.is_none() && run.in_project(), "manifests stay on");
        assert!(!quiet.join(".codex-img").exists() && !registry.exists());

        let forced = global.with_file_name("forced.ndjson");
        start(&quiet, Some(forced.to_str().unwrap()));
        assert_eq!(lines(&forced).len(), 2, "an explicit path beats the project");

        let loud = project();
        std::fs::write(loud.join("codex-img.json"), r#"{"events": true}"#).unwrap();
        assert!(start(&loud, Some("off")).0.log.is_none(), "env off beats the project");
        std::fs::write(&config, r#"{"events": false}"#).unwrap();
        assert!(start(&loud, None).0.log.as_ref().unwrap().is_file(), "the project beats the global file");

        let plain = project();
        assert!(start(&plain, None).0.log.is_none(), "a project that doesn't say follows the global file");
        assert!(start(&temp_dir("loose-off"), None).0.log.is_none(), "so do loose runs");
        assert!(!plain.join(".codex-img").exists() && !global.exists());
        std::fs::write(&config, r#"{"styles": {}}"#).unwrap();
        assert!(start(&plain, None).0.log.as_ref().unwrap().is_file());
        std::fs::write(plain.join("codex-img.json"), "not JSON").unwrap();
        assert!(start(&plain, None).0.log.as_ref().unwrap().is_file(), "an unreadable file logs as before");
    }

    #[test]
    fn project_files_never_rotate_and_loose_runs_use_only_the_global_log() {
        let root = project();
        let run = Run::start_at(&root, None, None, None, "run", 1, json!({}));
        let log = run.0.log.as_ref().unwrap();
        std::fs::write(log, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        run.end(None);
        assert!(std::fs::metadata(log).unwrap().len() > MAX_BYTES);
        assert!(!log.with_extension("ndjson.1").exists());
        let loose = temp_dir("loose");
        let global = temp_dir("loose-global").join("events.ndjson");
        Run::start_at(&loose, None, Some(global.clone()), None, "run", 1, json!({})).end(None);
        assert!(lines(&global)[0].get("root").is_none());
        assert!(!global.with_file_name("projects.json").exists());
        assert!(!loose.join(".codex-img").exists());
    }

    #[test]
    fn an_unwritable_log_never_fails_the_job() {
        let blocker = temp_dir("blocked").join("file");
        std::fs::write(&blocker, "not a folder").unwrap();
        let run = Run::start_in(Some(blocker.join("events.ndjson")), "run", 1, json!({}));
        let job = run.job(json!({}));
        job.stage("generating");
        job.done(json!({}));
        run.end(None);
    }
}
