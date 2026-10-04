#[test]
fn core_has_no_network_or_authentication_dependencies() {
    let output = std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["tree", "-p", "codex-img-core", "--offline", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR")).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let tree = String::from_utf8(output.stdout).unwrap();
    for line in tree.lines() {
        let name = line.split_whitespace().next().unwrap_or_default();
        assert!(!["reqwest", "ureq", "hyper", "hyper-util", "rustls", "native-tls", "openssl", "base64", "jsonwebtoken", "oauth2", "keyring", "codex-img"].contains(&name), "core has a transport/auth dependency: {line}");
    }
    // JSON is a neutral serialization dependency shared with auth; the auth code itself
    // and its token/base64 handling must stay outside this crate.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for forbidden in ["crate::auth", "crate::backend", "Credentials", "access_token", "auth.json"] {
            assert!(!text.contains(forbidden), "{} imports auth/backend: {forbidden}", entry.path().display());
        }
    }
}
