#[test]
fn broken_review_files_warn_without_breaking_ndjson_or_exit_status() {
    let root = std::env::temp_dir().join(format!("codex-img-review-integration-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("good.png.json"), r#"{"comment":"Good","star":true}"#).unwrap();
    std::fs::write(root.join("bad.png.json"), "{").unwrap();
    std::fs::write(root.join("bad-spec.json"), r#"{"raw_dir":5,"assets":{"hero":{"comment":"Bad"}}}"#).unwrap();
    for command in ["comments", "stars"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img"))
            .args([command, "--json"]).arg(&root).env("CODEX_IMG_EVENTS", "off").output().unwrap();
        assert!(output.status.success());
        let lines: Vec<_> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect();
        assert_eq!(lines.len(), 1);
        let warning = String::from_utf8(output.stderr).unwrap();
        assert!(warning.contains("bad.png.json"));
        if command == "comments" { assert!(warning.contains("bad-spec.json") && warning.contains("raw_dir")); }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_listing_warns_only_for_manifests_and_recognizable_batch_specs() {
    let root = std::env::temp_dir().join(format!("codex-img-review-noise-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (name, text) in [
        ("good.png.json", r#"{"comment":"Good","star":true}"#),
        ("bad.png.json", "{"),
        ("bad-spec.json", r#"{"raw_dir":"raw","assets":{"hero": "#),
        ("tsconfig.json", "{ // a normal JSONC config\n\"compilerOptions\": {} }"),
        ("package.json", r#"{"description":"assets",broken}"#),
        ("nested-config.json", r#"{"config":{"assets":{}},broken}"#),
        ("array-config.json", "[]"),
    ] { std::fs::write(root.join(name), text).unwrap(); }
    for command in ["comments", "stars"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img"))
            .args([command, "--json"]).arg(&root).env("CODEX_IMG_EVENTS", "off").output().unwrap();
        assert!(output.status.success());
        let values: Vec<_> = String::from_utf8(output.stdout).unwrap().lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect();
        assert_eq!(values.len(), 1);
        let warning = String::from_utf8(output.stderr).unwrap();
        assert!(warning.contains("bad.png.json"));
        assert_eq!(warning.contains("bad-spec.json"), command == "comments");
        for unrelated in ["tsconfig.json", "package.json", "nested-config.json", "array-config.json"] {
            assert!(!warning.contains(unrelated), "unexpected warning for {unrelated}: {warning}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn convert_only_busy_spec_fails_once_before_worker_output() {
    use fs2::FileExt;
    let root = std::env::temp_dir().join(format!("codex-img-convert-lock-{}", std::process::id()));
    std::fs::create_dir_all(root.join("raw")).unwrap();
    std::fs::write(root.join("codex-img.json"), "{}").unwrap();
    std::fs::write(root.join("assets.json"), r#"{"assets":{"hero":{"prompt":"Fox"},"coin":{"prompt":"Coin"}}}"#).unwrap();
    for name in ["hero", "coin"] {
        std::fs::write(root.join("raw").join(format!("{name}.png")), include_bytes!("fixtures/sprite.png")).unwrap();
    }
    let command = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img"));
        command.current_dir(&root).args(["batch", "assets.json", "--convert-only", "--no-wait", "--json"])
            .env("CODEX_IMG_EVENTS", "off");
        command
    };
    assert!(command().output().unwrap().status.success());
    std::fs::remove_dir_all(root.join("out")).unwrap();
    let path = std::fs::read_dir(root.join(".codex-img/locks")).unwrap().next().unwrap().unwrap().path();
    let held = std::fs::File::open(path).unwrap(); held.lock_exclusive().unwrap();
    let output = command().output().unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty(), "no per-asset failure lines");
    let message = String::from_utf8(output.stderr).unwrap();
    assert_eq!(message.matches("Another batch run holds the lock").count(), 1);
    assert!(!root.join("out").exists(), "no conversion starts while busy");
    drop(held);
    assert!(command().output().unwrap().status.success());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inspect_reports_edited_and_dry_run_does_not_suggest_reroll_for_missing_history() {
    let root = std::env::temp_dir().join(format!("codex-img-edited-integration-{}", std::process::id()));
    std::fs::create_dir_all(root.join("raw")).unwrap();
    std::fs::write(root.join("assets.json"), r#"{"assets":{"hero":{"prompt":"Fox","publish":false}}}"#).unwrap();
    std::fs::write(root.join("raw/hero.png"), b"unused fixture").unwrap();
    std::fs::write(root.join("raw/hero.png.json"), r#"{"fromComment":"Blue","parent":"missing.png","prompt":"Change only: Blue"}"#).unwrap();
    for mode in ["--inspect", "--dry-run"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img"))
            .current_dir(&root).args(["batch", "assets.json", mode, "--json"]).env("CODEX_IMG_EVENTS", "off").output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let value: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
        if mode == "--inspect" { assert_eq!(value["edited"], true); assert_eq!(value["changed"], false); }
        assert!(!stdout.contains("--reroll"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inspect_excludes_generated_but_unactivated_pending_versions() {
    let root = std::env::temp_dir().join(format!("codex-img-pending-integration-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".codex-img/history/hero")).unwrap();
    std::fs::write(root.join("assets.json"), r#"{"assets":{"hero":{"prompt":"Fox"}}}"#).unwrap();
    for name in ["old.png", "unactivated.pending.png"] { std::fs::write(root.join(".codex-img/history/hero").join(name), b"unused fixture").unwrap(); }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img")).current_dir(&root)
        .args(["batch", "assets.json", "--inspect", "--json"]).env("CODEX_IMG_EVENTS", "off").output().unwrap();
    assert!(output.status.success()); let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["history"].as_array().unwrap().len(), 1); assert!(value["history"][0]["path"].as_str().unwrap().ends_with("old.png"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loose_review_edits_use_global_state_lock_even_with_events_off() {
    let root = std::env::temp_dir().join(format!("codex-img-loose-lock-integration-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap(); std::fs::write(root.join("image.png.json"), "{}").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img")).current_dir(&root)
        .args(["comments", "image.png", "--set=Keep this", "--json"])
        .env("CODEX_IMG_EVENTS", "off").env("XDG_STATE_HOME", root.join("state")).output().unwrap();
    assert!(output.status.success()); assert!(!root.join(".codex-img").exists()); assert!(root.join("state/codex-img/review.lock").is_file());
    std::fs::remove_dir_all(root).unwrap();
}
