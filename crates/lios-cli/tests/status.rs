use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use uuid::Uuid;

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lios-cli-status-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn status_reports_a_fresh_home_without_initializing_it() {
    let home = TempHome::new();

    let output = Command::new(env!("CARGO_BIN_EXE_lios"))
        .args(["--home", home.path().to_str().unwrap(), "status"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("State: not initialized"),
        "unexpected status output: {stdout}"
    );
    assert!(
        stdout.contains("Recovery key: missing"),
        "unexpected status output: {stdout}"
    );
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}
