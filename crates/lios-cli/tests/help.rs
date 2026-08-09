use std::process::Command;

#[test]
fn help_and_version_exit_successfully() {
    for args in [["--help"], ["--version"], ["help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_lios"))
            .args(args)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "lios {} exited with {:?}: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn invalid_command_keeps_usage_exit_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_lios"))
        .arg("not-a-lios-command")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}
