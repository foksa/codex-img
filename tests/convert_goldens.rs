//! Compatibility baseline captured before the core extraction. No backend or login is used.
#[test]
fn convert_matches_every_pre_core_golden_byte_for_byte() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/convert");
    let cases: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(fixtures.join("cases.json")).unwrap()).unwrap();
    let outputs = std::env::temp_dir().join(format!("codex-img-goldens-{}", std::process::id()));
    std::fs::create_dir_all(&outputs).unwrap();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = fixtures.join(case["output"].as_str().unwrap());
        let output = outputs.join(expected.file_name().unwrap());
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_codex-img"));
        command.arg("convert").arg(fixtures.join(case["input"].as_str().unwrap()))
            .arg("-o").arg(&output).args(["--quiet", "--json"]).env("CODEX_IMG_EVENTS", "off");
        for arg in case["args"].as_array().unwrap() { command.arg(arg.as_str().unwrap()); }
        let mask = case["mask"].as_str().map(|_| outputs.join(format!("{name}-mask.png")));
        if let Some(mask) = &mask { command.arg(format!("--mask-out={}", mask.display())); }
        let result = command.output().unwrap();
        assert!(result.status.success(), "{name}: {}", String::from_utf8_lossy(&result.stderr));
        assert_eq!(std::fs::read(&output).unwrap(), std::fs::read(expected).unwrap(), "{name}");
        if let Some(mask) = mask { assert_eq!(std::fs::read(mask).unwrap(), std::fs::read(fixtures.join(case["mask"].as_str().unwrap())).unwrap(), "{name} mask"); }
        let mut report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        for key in ["path", "input", "durationMs", "maskPath"] { report.as_object_mut().unwrap().remove(key); }
        assert_eq!(report, case["report"], "{name} report");
    }
    std::fs::remove_dir_all(outputs).unwrap();
}
