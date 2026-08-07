use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use uuid::Uuid;

#[test]
fn worker_status_and_stop_use_the_same_lios_home() {
    let home = std::env::temp_dir().join(format!("lios-worker-test-{}", Uuid::new_v4()));
    fs::create_dir(&home).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_lios-worker"))
        .args(["--home", home.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = Command::new(env!("CARGO_BIN_EXE_lios"))
            .args([
                "--home",
                home.to_str().unwrap(),
                "--json",
                "worker",
                "status",
            ])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&status.stdout).contains("\"running\":true") {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not start");
        thread::sleep(Duration::from_millis(50));
    }

    let stop = Command::new(env!("CARGO_BIN_EXE_lios"))
        .args(["--home", home.to_str().unwrap(), "worker", "stop"])
        .output()
        .unwrap();
    assert!(stop.status.success());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if worker.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not stop");
        thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_dir_all(home);
}
