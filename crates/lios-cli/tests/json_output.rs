use std::fs;
use std::process::Command;

use serde_json::Value;
use uuid::Uuid;

fn temp_home() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("lios-cli-json-{}", Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn json_success_is_one_stable_document() {
    let home = temp_home();
    let output = Command::new(env!("CARGO_BIN_EXE_lios"))
        .args(["--home", home.to_str().unwrap(), "--json", "status"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["ok"], true);
    assert_eq!(document["command"], "status");
    assert!(document["result"].is_object());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn json_input_error_uses_exit_two_without_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_lios"))
        .args(["--json", "upload"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], false);
    assert_eq!(document["command"], "parse");
    assert_eq!(document["error"]["code"], "invalid_input");
}
