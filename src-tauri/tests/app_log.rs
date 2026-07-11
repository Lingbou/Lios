#[allow(dead_code)]
#[path = "../src/app_log.rs"]
mod app_log;

use std::fs;

use app_log::AppLogger;
use chrono::{TimeZone, Utc};
use lios_core::config::LiosPaths;
use serde_json::{json, Value};
use tempfile::tempdir;

fn timestamp(day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, day, 12, 34, 56)
        .single()
        .unwrap()
}

fn string_values(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                string_values(value, output);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                string_values(value, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[test]
fn writes_parseable_redacted_json_lines_and_uses_daily_files() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    let logger = AppLogger::new(&paths);
    let home = temp.path().display().to_string();
    let home_slashes = home.replace('\\', "/");
    let lios = paths.home.display().to_string();
    let lios_slashes = lios.replace('\\', "/");

    logger
        .write_at(
            timestamp(11),
            "info",
            "redaction_probe",
            json!({
                "nested": {
                    "token": "ms-token-super-secret",
                    "headers": "Authorization: Custom auth-secret; secondary-auth-secret\r\nCookie: session=cookie-secret; preference=preference-secret\nSet-Cookie: refresh=set-cookie-secret; Path=/; HttpOnly\r\nX-Trace: visible",
                    "signed_url": "https://uploads.example.test/file?X-Amz-Credential=aws-credential-secret&X-Amz-Signature=aws-signature-secret&token=upload-url-secret&sig=short-signature-secret&X-Goog-Credential=google-credential-secret&x-goog-signature=google-signature-secret&Policy=policy-secret&Key-Pair-Id=key-pair-secret&GoogleAccessId=google-access-secret&OSSAccessKeyId=oss-access-secret&client_secret=client-query-secret&Api_Key=api-query-secret&PASSWORD=password-query-secret&secret=plain-query-secret&secret_key=secret-key-query-secret&Private_Key=private-key-query-secret",
                    "home_backslashes": format!("{home}\\Documents\\source.bin"),
                    "home_slashes": format!("{home_slashes}/Documents/source.bin"),
                    "lios_backslashes": format!("{lios}\\staging\\task.bin"),
                    "lios_slashes": format!("{lios_slashes}/staging/task.bin")
                }
            }),
        )
        .unwrap();
    logger
        .write_at(timestamp(12), "warn", "next_day", json!({ "attempt": 2 }))
        .unwrap();

    let first_path = paths.logs.join("lios-2026-07-11.log");
    let second_path = paths.logs.join("lios-2026-07-12.log");
    assert!(first_path.is_file());
    assert!(second_path.is_file());
    assert_ne!(first_path, second_path);

    let raw = fs::read_to_string(first_path).unwrap();
    let record: Value = serde_json::from_str(raw.trim_end()).unwrap();
    assert_eq!(record["timestamp"], "2026-07-11T12:34:56Z");
    assert_eq!(record["level"], "info");
    assert_eq!(record["event"], "redaction_probe");
    assert_eq!(
        record["details"]["nested"]["headers"],
        "[REDACTED]\r\n[REDACTED]\n[REDACTED]\r\nX-Trace: visible"
    );
    let signed_url = record["details"]["nested"]["signed_url"].as_str().unwrap();
    for parameter in [
        "?X-Amz-Credential=[REDACTED]",
        "&X-Amz-Signature=[REDACTED]",
        "&token=[REDACTED]",
        "&sig=[REDACTED]",
        "&X-Goog-Credential=[REDACTED]",
        "&x-goog-signature=[REDACTED]",
        "&Policy=[REDACTED]",
        "&Key-Pair-Id=[REDACTED]",
        "&GoogleAccessId=[REDACTED]",
        "&OSSAccessKeyId=[REDACTED]",
        "&client_secret=[REDACTED]",
        "&Api_Key=[REDACTED]",
        "&PASSWORD=[REDACTED]",
        "&secret=[REDACTED]",
        "&secret_key=[REDACTED]",
        "&Private_Key=[REDACTED]",
    ] {
        assert!(signed_url.contains(parameter), "{signed_url}");
    }

    let mut strings = Vec::new();
    string_values(&record, &mut strings);
    let rendered = strings.join("\n");
    let normalized = rendered.replace('\\', "/");
    for secret in [
        "ms-token-super-secret",
        "auth-secret",
        "secondary-auth-secret",
        "cookie-secret",
        "preference-secret",
        "set-cookie-secret",
        "aws-credential-secret",
        "aws-signature-secret",
        "upload-url-secret",
        "short-signature-secret",
        "google-credential-secret",
        "google-signature-secret",
        "policy-secret",
        "key-pair-secret",
        "google-access-secret",
        "oss-access-secret",
        "client-query-secret",
        "api-query-secret",
        "password-query-secret",
        "plain-query-secret",
        "secret-key-query-secret",
        "private-key-query-secret",
    ] {
        assert!(!rendered.contains(secret), "{rendered}");
    }
    assert!(!normalized.contains(&home_slashes), "{normalized}");
    assert!(!normalized.contains(&lios_slashes), "{normalized}");
    assert!(
        normalized.contains("~/.lios/staging/task.bin"),
        "{normalized}"
    );
    assert!(
        normalized.contains("~/Documents/source.bin"),
        "{normalized}"
    );
}

#[test]
fn prunes_only_exact_log_files_outside_the_seven_day_window() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    fs::create_dir_all(&paths.logs).unwrap();
    for day in 1..=11 {
        fs::write(
            paths.logs.join(format!("lios-2026-07-{day:02}.log")),
            "{}\n",
        )
        .unwrap();
    }
    for unrelated in [
        "notes.log",
        "lios-2026-7-01.log",
        "lios-2026-07-01.log.bak",
        "lios-2026-07-99.log",
    ] {
        fs::write(paths.logs.join(unrelated), "leave me").unwrap();
    }

    AppLogger::new(&paths)
        .write_at(timestamp(12), "info", "retention_probe", json!({}))
        .unwrap();

    for day in 1..=5 {
        assert!(!paths
            .logs
            .join(format!("lios-2026-07-{day:02}.log"))
            .exists());
    }
    for day in 6..=12 {
        assert!(paths
            .logs
            .join(format!("lios-2026-07-{day:02}.log"))
            .is_file());
    }
    for unrelated in [
        "notes.log",
        "lios-2026-7-01.log",
        "lios-2026-07-01.log.bak",
        "lios-2026-07-99.log",
    ] {
        assert!(paths.logs.join(unrelated).is_file(), "{unrelated}");
    }
}

#[test]
fn redacts_nested_sensitive_keys_for_every_value_type_without_redacting_normal_ms_ids() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    let logger = AppLogger::new(&paths);
    let realistic_token = ["ms-", "abcdefghijklmnopqrstuvwxyz", "0123456789"].concat();

    logger
        .write_at(
            timestamp(12),
            "info",
            "structured_redaction_probe",
            json!({
                "accessKeyId": 42,
                "secret": { "nested": "plain-structured-secret" },
                "secret_key": ["structured-secret-key"],
                "nested": [
                    {
                        "secretAccessKey": false,
                        "apiKey": null,
                        "clientSecret": ["client-secret", { "still_secret": true }]
                    },
                    {
                        "sessionToken": { "raw": "session-secret" },
                        "security_token": 7,
                        "password": ["password-secret"],
                        "privateKey": { "pem": "private-key-secret" }
                    },
                    {
                        "ordinary": ["ms-windows", "ms-settings"],
                        "message": format!("issued {realistic_token}")
                    }
                ]
            }),
        )
        .unwrap();

    let raw = fs::read_to_string(paths.logs.join("lios-2026-07-12.log")).unwrap();
    let record: Value = serde_json::from_str(raw.trim_end()).unwrap();
    let redacted = Value::String("[REDACTED]".to_string());
    assert_eq!(record["details"]["accessKeyId"], redacted);
    assert_eq!(record["details"]["secret"], redacted);
    assert_eq!(record["details"]["secret_key"], redacted);
    assert_eq!(record["details"]["nested"][0]["secretAccessKey"], redacted);
    assert_eq!(record["details"]["nested"][0]["apiKey"], redacted);
    assert_eq!(record["details"]["nested"][0]["clientSecret"], redacted);
    assert_eq!(record["details"]["nested"][1]["sessionToken"], redacted);
    assert_eq!(record["details"]["nested"][1]["security_token"], redacted);
    assert_eq!(record["details"]["nested"][1]["password"], redacted);
    assert_eq!(record["details"]["nested"][1]["privateKey"], redacted);
    assert_eq!(record["details"]["nested"][2]["ordinary"][0], "ms-windows");
    assert_eq!(record["details"]["nested"][2]["ordinary"][1], "ms-settings");
    assert_eq!(
        record["details"]["nested"][2]["message"],
        "issued [REDACTED]"
    );
    for secret in [
        "client-secret",
        "session-secret",
        "password-secret",
        "private-key-secret",
        "plain-structured-secret",
        "structured-secret-key",
        realistic_token.as_str(),
    ] {
        assert!(!raw.contains(secret), "{raw}");
    }
}

#[cfg(windows)]
#[test]
fn appends_current_record_when_an_expired_log_is_locked_against_deletion() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    fs::create_dir_all(&paths.logs).unwrap();
    let expired = paths.logs.join("lios-2026-07-01.log");
    let later_expired = paths.logs.join("lios-2026-07-02.log");
    fs::write(&expired, "locked expired\n").unwrap();
    fs::write(&later_expired, "removable expired\n").unwrap();
    let _locked = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&expired)
        .unwrap();

    AppLogger::new(&paths)
        .write_at(timestamp(12), "info", "retention_locked", json!({}))
        .unwrap();

    let current = fs::read_to_string(paths.logs.join("lios-2026-07-12.log")).unwrap();
    assert!(
        current.contains("\"event\":\"retention_locked\""),
        "{current}"
    );
    assert!(expired.is_file());
    assert!(!later_expired.exists());
}
