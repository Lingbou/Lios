use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lios_core::config::LiosPaths;
use lios_core::space_lock::SpaceLockError;
use tempfile::tempdir;

const ACTION_ENV: &str = "LIOS_SPACE_LOCK_TEST_ACTION";
const HOME_ENV: &str = "LIOS_SPACE_LOCK_TEST_HOME";
const SPACE_ENV: &str = "LIOS_SPACE_LOCK_TEST_SPACE";
const READY_ENV: &str = "LIOS_SPACE_LOCK_TEST_READY";

#[test]
fn invalid_space_ids_are_rejected_before_touching_disk() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    let invalid_ids = [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        format!("{}../x", "a".repeat(60)),
    ];

    for space_id in invalid_ids {
        assert!(matches!(
            paths.try_lock_space(&space_id),
            Err(SpaceLockError::InvalidSpaceId)
        ));
    }
    assert!(!paths.home.exists());
}

#[test]
fn locks_are_cross_process_scoped_and_reusable() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    let space_a = "a".repeat(64);
    let space_b = "b".repeat(64);
    let mut child = HeldLockProcess::spawn(tmp.path(), &space_a);

    match paths.try_lock_space(&space_a) {
        Err(SpaceLockError::Busy { space_id }) => assert_eq!(space_id, space_a),
        other => panic!("same-space lock attempt should be busy, got {other:?}"),
    }

    let other_space = paths.try_lock_space(&space_b).unwrap();
    assert!(other_space.path().exists());
    drop(other_space);

    child.release();
    let first = paths.try_lock_space(&space_a).unwrap();
    let lock_path = first.path().to_path_buf();
    drop(first);
    assert!(
        lock_path.exists(),
        "lock files must remain after guard drop"
    );

    let second = paths.try_lock_space(&space_a).unwrap();
    drop(second);
    assert!(
        lock_path.exists(),
        "reusable lock file must not be unlinked"
    );
}

#[test]
fn operating_system_releases_lock_when_process_exits() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    let space_id = "c".repeat(64);
    let status = helper_command(tmp.path(), &space_id, "exit", None)
        .status()
        .unwrap();
    assert!(status.success(), "lock helper failed with {status}");

    let lock = paths.try_lock_space(&space_id).unwrap();
    drop(lock);
}

#[cfg(unix)]
#[test]
fn lock_tree_is_owner_only_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    let lock = paths.try_lock_space(&"d".repeat(64)).unwrap();

    for directory in [
        paths.home.clone(),
        paths.home.join("locks"),
        paths.home.join("locks/spaces"),
    ] {
        let mode = fs::metadata(directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
    let mode = fs::metadata(lock.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

// This test is also the entrypoint used by the parent tests above. With no
// helper environment it is a no-op in the ordinary test-suite process.
#[test]
fn space_lock_subprocess_helper() {
    let Ok(action) = env::var(ACTION_ENV) else {
        return;
    };
    let home = env::var_os(HOME_ENV).expect("helper home is configured");
    let space_id = env::var(SPACE_ENV).expect("helper space is configured");
    let paths = LiosPaths::from_home(home);
    let _lock = paths.try_lock_space(&space_id).unwrap();

    match action.as_str() {
        "hold" => {
            let ready = env::var_os(READY_ENV).expect("helper ready path is configured");
            fs::write(ready, b"locked").unwrap();
            let mut release = [0_u8; 1];
            io::stdin().read_exact(&mut release).unwrap();
        }
        "exit" => process::exit(0),
        other => panic!("unknown helper action: {other}"),
    }
}

struct HeldLockProcess {
    child: Child,
}

impl HeldLockProcess {
    fn spawn(home: &Path, space_id: &str) -> Self {
        let ready = home.join("space-lock-ready");
        let child = helper_command(home, space_id, "hold", Some(&ready))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut held = Self { child };
        held.wait_until_ready(&ready);
        held
    }

    fn wait_until_ready(&mut self, ready: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if ready.exists() {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("lock helper exited before acquiring the lock: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "lock helper did not acquire the lock in time"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn release(&mut self) {
        let status = self.stop().unwrap();
        assert!(status.success(), "lock helper failed with {status}");
    }

    fn stop(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        if let Some(mut stdin) = self.child.stdin.take() {
            stdin.write_all(b"x")?;
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                return self.child.wait();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for HeldLockProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn helper_command(home: &Path, space_id: &str, action: &str, ready: Option<&Path>) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("space_lock_subprocess_helper")
        .arg("--nocapture")
        .env(ACTION_ENV, action)
        .env(HOME_ENV, home)
        .env(SPACE_ENV, space_id)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(ready) = ready {
        command.env(READY_ENV, ready);
    }
    command
}
