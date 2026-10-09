use butterpollo_core::state::{Credentials, load_json};
use serde_json::json;

#[test]
fn creds_sets_sign_in_only_in_the_explicit_profile() {
    let directory =
        std::env::temp_dir().join(format!("butterpollo-creds-parity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_butterpollo"))
        .arg("--config-dir")
        .arg(&directory)
        .args(["--creds", "parity", "parity-test-password"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let credentials = Credentials::load(&directory.join("sunshine_state.json"))
        .unwrap()
        .unwrap();
    assert!(credentials.verifies("PARITY", "parity-test-password"));
    assert!(!credentials.verifies("parity", "wrong-password"));
    let saved = load_json(&directory.join("sunshine_state.json"), json!({})).unwrap();
    assert_ne!(saved["password"], "parity-test-password");
    assert!(saved["root"]["named_devices"].is_array());
    std::fs::remove_dir_all(&directory).unwrap();
}
