//! Project folders share the preset discovery rule; output locations never choose the project.
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn root(cwd: &Path) -> Option<PathBuf> {
    crate::presets::find_project(cwd).and_then(|file| file.parent().map(crate::batch::resolved))
}

/// Resolve first, so a symlink escaping the project stays absolute. Paths inside the project
/// use `/` on every platform, so run files stay portable; outside paths stay native.
pub fn shown(path: &Path, root: Option<&Path>) -> String {
    let absolute = crate::batch::resolved(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    match root.and_then(|root| absolute.strip_prefix(root).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => ".".into(),
        Some(relative) => relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"),
        None => absolute.display().to_string(),
    }
}

pub fn prepare(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root.join(".codex-img/runs"))?;
    // This belongs to the user once created. Keep edits, even if several runs start together.
    use std::io::Write;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(root.join(".codex-img/README.md")) {
        Ok(mut file) => file.write_all(README.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

const README: &str = "# codex-img project files\n\n\
This folder holds one event file per run in runs/, and earlier batch images in\n\
history/ when re-rolling assets. Run files store prompts and paths in plain text.\n\n\
Committing this folder (or just part of it) is your choice. If you prefer to keep\n\
it local, add this line to your project's .gitignore:\n\n\
```\n.codex-img/\n```\n\n\
codex-img never edits .gitignore itself.\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOptions {
    pub folder: Option<String>,
    pub no_events: bool,
    pub json: bool,
}

pub fn init_help() -> &'static str {
    "Usage: codex-img init [folder] [--no-events] [--json]\n\n\
Start a project: write codex-img.json in the folder (default: current; created if missing).\n\
Refuses inside an existing project and never overwrites.\n\
--no-events writes {\"events\": false}: no run history in .codex-img/runs/ and no entry in the\n\
projects.json registry. Manifests are still written.\n\
--json prints {file, root, events}."
}

pub fn parse_init(args: &[String]) -> Result<Option<InitOptions>> {
    let mut opts = InitOptions { folder: None, no_events: false, json: false };
    let mut literal = false;
    for arg in args {
        match arg.as_str() {
            "--" if !literal => literal = true,
            "--no-events" if !literal => opts.no_events = true,
            "--json" if !literal => opts.json = true,
            "--help" | "-h" if !literal => return Ok(None),
            value if !literal && value.starts_with('-') => return Err(Error::usage(format!("Unknown init option: {value}"))),
            _ => {
                if opts.folder.replace(arg.clone()).is_some() {
                    return Err(Error::usage("init takes one folder."));
                }
            }
        }
    }
    Ok(Some(opts))
}

pub fn run_init(opts: &InitOptions) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::other(e.to_string()))?;
    let made = init(opts, &cwd)?;
    if opts.json {
        println!("{made}");
    } else {
        let off = if opts.no_events { " (run logging off)" } else { "" };
        println!("Created {}{off}.", made["file"].as_str().unwrap_or_default());
        println!("Run history goes in .codex-img/; add it to .gitignore to keep it out of git.");
    }
    Ok(0)
}

fn init(opts: &InitOptions, cwd: &Path) -> Result<Value> {
    let folder = std::path::absolute(cwd.join(opts.folder.as_deref().unwrap_or("."))).map_err(|e| Error::other(e.to_string()))?;
    let file = folder.join(crate::presets::PROJECT_FILE);
    if let Some(existing) = crate::presets::find_project(&folder) {
        return Err(Error::usage(if existing == file {
            format!("{} already exists; codex-img init never overwrites.", file.display())
        } else {
            format!("{} is already inside the project of {}.", folder.display(), existing.display())
        }));
    }
    if folder.exists() && !folder.is_dir() {
        return Err(Error::usage(format!("{} is not a folder.", folder.display())));
    }
    let text = if opts.no_events { "{\n  \"events\": false\n}\n" } else { "{}\n" };
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&folder)?;
        std::fs::OpenOptions::new().write(true).create_new(true).open(&file)?.write_all(text.as_bytes())
    };
    write().map_err(|e| Error::other(format!("Unable to write {}: {e}", file.display())))?;
    let root = crate::batch::resolved(&folder);
    Ok(json!({"file": root.join(crate::presets::PROJECT_FILE).display().to_string(), "root": root.display().to_string(), "events": !opts.no_events}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_starts_a_project_once_and_never_nests_or_overwrites() {
        let dir = crate::auth::tests::temp_dir("init").canonicalize().unwrap();
        let opts = |folder: &str, no_events| InitOptions { folder: Some(folder.into()), no_events, json: true };
        let made = init(&opts("game", false), &dir).unwrap();
        assert_eq!(made, json!({"file": dir.join("game/codex-img.json").display().to_string(), "root": dir.join("game").display().to_string(), "events": true}));
        assert_eq!(std::fs::read_to_string(dir.join("game/codex-img.json")).unwrap(), "{}\n");
        assert_eq!(root(&dir.join("game")), Some(dir.join("game")));
        assert!(crate::check::validate(&dir.join("game/codex-img.json")).is_empty());
        assert!(!dir.join("game/.codex-img").exists() && !dir.join("game/.gitignore").exists());

        std::fs::write(dir.join("game/codex-img.json"), "{\"styles\": {}}").unwrap();
        let again = init(&opts(".", true), &dir.join("game")).unwrap_err();
        assert!(again.message.contains("already exists"), "{}", again.message);
        assert_eq!(std::fs::read_to_string(dir.join("game/codex-img.json")).unwrap(), "{\"styles\": {}}");
        let nested = init(&opts("game/art", false), &dir).unwrap_err();
        assert!(nested.message.contains("already inside the project of") && nested.message.contains(&dir.join("game/codex-img.json").display().to_string()));
        assert!(!dir.join("game/art").exists());

        let quiet = init(&opts("quiet", true), &dir).unwrap();
        assert_eq!(quiet["events"], false);
        assert_eq!(crate::presets::events_setting(&dir.join("quiet/codex-img.json")), Some(false));
        assert!(crate::check::validate(&dir.join("quiet/codex-img.json")).is_empty());
        std::fs::write(dir.join("file"), "").unwrap();
        assert!(init(&opts("file", false), &dir).unwrap_err().message.contains("not a folder"));
    }

    #[test]
    fn parses_init() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_init(&args(&[])).unwrap(), Some(InitOptions { folder: None, no_events: false, json: false }));
        assert_eq!(parse_init(&args(&["art", "--no-events", "--json"])).unwrap(), Some(InitOptions { folder: Some("art".into()), no_events: true, json: true }));
        assert_eq!(parse_init(&args(&["-h"])).unwrap(), None);
        assert!(parse_init(&args(&["a", "b"])).is_err() && parse_init(&args(&["--force"])).is_err());
        assert_eq!(parse_init(&args(&["--", "--odd"])).unwrap().unwrap().folder.as_deref(), Some("--odd"));
    }

    #[test]
    fn discovers_the_nearest_project_and_preserves_its_readme() {
        let dir = crate::auth::tests::temp_dir("project").canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("art/nested/work")).unwrap();
        assert_eq!(root(&dir), None);
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        assert_eq!(root(&dir.join("art/nested/work")), Some(dir.clone()));
        std::fs::write(dir.join("art/codex-img.json"), "{}").unwrap();
        assert_eq!(root(&dir.join("art/nested/work")), Some(dir.join("art")));
        prepare(&dir).unwrap();
        assert!(dir.join(".codex-img/runs").is_dir());
        assert!(std::fs::read_to_string(dir.join(".codex-img/README.md")).unwrap().contains(".gitignore"));
        std::fs::write(dir.join(".codex-img/README.md"), "My notes").unwrap();
        prepare(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".codex-img/README.md")).unwrap(), "My notes");
        assert!(!dir.join(".gitignore").exists());
    }

    #[test]
    fn paths_inside_are_relative_and_paths_outside_are_absolute() {
        let dir = crate::auth::tests::temp_dir("project-paths").canonicalize().unwrap();
        let root = dir.join("game");
        std::fs::create_dir_all(root.join("art")).unwrap();
        assert_eq!(shown(&root, Some(&root)), ".");
        assert_eq!(shown(&root.join("art/../new.png"), Some(&root)), "new.png");
        assert_eq!(shown(&dir.join("other.png"), Some(&root)), dir.join("other.png").display().to_string());
        assert_eq!(shown(&root.join("new.png"), None), root.join("new.png").display().to_string());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, root.join("external")).unwrap();
            assert_eq!(shown(&root.join("external/new.png"), Some(&root)), dir.join("new.png").display().to_string());
        }
    }
}
