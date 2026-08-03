use std::path::PathBuf;

use lios_application::location::{Location, LocationParser};
use lios_application::CommandErrorCode;

#[test]
fn parses_remote_roots_absolute_paths_and_preserves_trailing_slashes() {
    let parser = LocationParser::new(["photos"]);

    assert_eq!(
        parser.parse("photos:").unwrap(),
        Location::remote("photos", "/", false)
    );
    assert_eq!(
        parser.parse("photos:/docs").unwrap(),
        Location::remote("photos", "/docs", false)
    );
    assert_eq!(
        parser.parse("photos:/docs/").unwrap(),
        Location::remote("photos", "/docs", true)
    );

    let error = parser.parse("photos:docs").unwrap_err();
    assert_eq!(error.code, CommandErrorCode::InvalidInput);
}

#[test]
fn colon_files_are_explicitly_local_and_windows_drive_letters_win() {
    let parser = LocationParser::new(["photos"]);

    assert_eq!(
        parser.parse("./photos:").unwrap(),
        Location::local(PathBuf::from("./photos:"), false)
    );
    assert_eq!(
        parser.parse(r"C:\backup\photos:").unwrap(),
        Location::local(PathBuf::from(r"C:\backup\photos:"), false)
    );
    assert_eq!(
        parser.parse("local-dir/").unwrap(),
        Location::local(PathBuf::from("local-dir"), true)
    );
}

#[test]
fn remote_syntax_requires_a_registered_space() {
    let parser = LocationParser::new(["photos"]);

    let error = parser.parse("archive:/docs").unwrap_err();

    assert_eq!(error.code, CommandErrorCode::InvalidInput);
    assert!(error.message.contains("not registered"));
}
