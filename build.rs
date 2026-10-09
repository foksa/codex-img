//! Local builds print their commit in --version (0.8.0+3f3ed81, or +3f3ed81.dirty with uncommitted
//! changes), so a recorded version names the exact encoder. A build of the release tag itself, or
//! one outside a git checkout (a crate download), prints the plain version.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    for path in [".git/HEAD", ".git/index", ".git/refs/tags", ".git/packed-refs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let tag = format!("v{version}");
    let full = match git(&["rev-parse", "--short=7", "HEAD"]) {
        Some(_) if !dirty && git(&["tag", "--points-at", "HEAD"]).is_some_and(|tags| tags.lines().any(|t| t == tag)) => version.to_string(),
        Some(commit) => format!("{version}+{commit}{}", if dirty { ".dirty" } else { "" }),
        None => version.to_string(),
    };
    println!("cargo:rustc-env=CODEX_IMG_VERSION={full}");
}
