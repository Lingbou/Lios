use std::fs;
use std::io;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use lios_core::config::LiosPaths;
use regex::{Captures, Regex, RegexBuilder};
use serde_json::{json, Value};

static HEADER_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:authorization|set-cookie|cookie)\s*:\s*[^\r\n]*")
        .expect("header redaction regex is valid")
});
static BEARER_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bbearer\s+[a-z0-9._~+/=-]+").expect("bearer redaction regex is valid")
});
static COOKIE_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bm_session_id\s*=\s*[^;\s,&]+").expect("cookie redaction regex is valid")
});
static MODELSCOPE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bms[-_][a-z0-9][a-z0-9._~+/-]{23,}\b")
        .expect("ModelScope token redaction regex is valid")
});
static SIGNED_QUERY_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([?&](?:x-amz-(?:credential|signature|security-token)|x-goog-(?:credential|signature)|credential|signature|sig|policy|key-pair-id|googleaccessid|token|access[_-]?key(?:id)?|ossaccesskeyid|client_secret|api_key|password|secret|secret_key|private_key)=)[^&#\s]+",
    )
    .expect("signed URL redaction regex is valid")
});

pub(crate) struct AppLogger {
    paths: LiosPaths,
    redactor: Redactor,
    write_lock: Mutex<()>,
}

struct Redactor {
    path_rules: Vec<(Regex, &'static str)>,
}

impl AppLogger {
    pub(crate) fn new(paths: &LiosPaths) -> Self {
        Self {
            paths: paths.clone(),
            redactor: Redactor::new(paths),
            write_lock: Mutex::new(()),
        }
    }

    pub(crate) fn log(&self, level: &str, event: &str, details: Value) {
        let _ = self.write_at(Utc::now(), level, event, details);
    }

    pub(crate) fn write_at(
        &self,
        timestamp: DateTime<Utc>,
        level: &str,
        event: &str,
        details: Value,
    ) -> io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| io::Error::other("app log mutex is poisoned"))?;

        let mut record = json!({
            "timestamp": timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
            "level": level,
            "event": event,
            "details": details,
        });
        self.redactor.sanitize_value(&mut record);
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        self.paths
            .append_private_log(timestamp.date_naive(), &line)?;
        let _ = self.prune(timestamp.date_naive());
        Ok(())
    }

    fn prune(&self, today: NaiveDate) -> io::Result<()> {
        let oldest = today - Duration::days(6);
        for entry in fs::read_dir(&self.paths.logs)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(date) = exact_log_date(&name) else {
                continue;
            };
            if date < oldest || date > today {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

impl Redactor {
    fn new(paths: &LiosPaths) -> Self {
        let mut path_rules = Vec::new();
        add_path_rules(&mut path_rules, &paths.home, "~/.lios");
        if let Some(user_home) = paths.home.parent() {
            add_path_rules(&mut path_rules, user_home, "~");
        }
        Self { path_rules }
    }

    fn sanitize_value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.sanitize_string(text),
            Value::Array(values) => {
                for value in values {
                    self.sanitize_value(value);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if sensitive_key(key) {
                        *value = Value::String("[REDACTED]".to_string());
                    } else {
                        self.sanitize_value(value);
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    fn sanitize_string(&self, value: &str) -> String {
        let mut sanitized = value.to_string();
        let mut path_replaced = false;
        for (pattern, replacement) in &self.path_rules {
            if pattern.is_match(&sanitized) {
                sanitized = pattern.replace_all(&sanitized, *replacement).into_owned();
                path_replaced = true;
            }
        }
        if path_replaced {
            sanitized = sanitized.replace('\\', "/");
        }
        sanitized = HEADER_SECRET
            .replace_all(&sanitized, "[REDACTED]")
            .into_owned();
        sanitized = BEARER_SECRET
            .replace_all(&sanitized, "Bearer [REDACTED]")
            .into_owned();
        sanitized = COOKIE_SECRET
            .replace_all(&sanitized, "m_session_id=[REDACTED]")
            .into_owned();
        sanitized = MODELSCOPE_TOKEN
            .replace_all(&sanitized, "[REDACTED]")
            .into_owned();
        SIGNED_QUERY_SECRET
            .replace_all(&sanitized, |captures: &Captures<'_>| {
                format!("{}[REDACTED]", &captures[1])
            })
            .into_owned()
    }
}

fn add_path_rules(rules: &mut Vec<(Regex, &'static str)>, path: &Path, replacement: &'static str) {
    let mut variants = path_variants(path);
    variants.sort_by_key(|variant| std::cmp::Reverse(variant.len()));
    variants.dedup();
    for variant in variants {
        if variant.is_empty() {
            continue;
        }
        let pattern = RegexBuilder::new(&regex::escape(&variant))
            .case_insensitive(cfg!(windows))
            .build()
            .expect("escaped path redaction regex is valid");
        rules.push((pattern, replacement));
    }
}

fn path_variants(path: &Path) -> Vec<String> {
    let raw = path.display().to_string();
    let stripped = raw.strip_prefix("\\\\?\\").unwrap_or(&raw).to_string();
    vec![
        stripped.clone(),
        stripped.replace('\\', "/"),
        stripped.replace('/', "\\"),
    ]
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "cookie"
            | "setcookie"
            | "token"
            | "accesstoken"
            | "modelscopetoken"
            | "credential"
            | "credentials"
            | "signature"
            | "accesskeyid"
            | "secretaccesskey"
            | "apikey"
            | "clientsecret"
            | "sessiontoken"
            | "securitytoken"
            | "password"
            | "privatekey"
            | "secret"
            | "secretkey"
    ) || normalized.ends_with("token")
        || normalized.ends_with("credential")
        || normalized.ends_with("signature")
        || normalized.ends_with("password")
        || normalized.ends_with("privatekey")
        || normalized.ends_with("apikey")
        || normalized.ends_with("clientsecret")
        || normalized.ends_with("accesskeyid")
        || normalized.ends_with("secretaccesskey")
}

fn exact_log_date(name: &str) -> Option<NaiveDate> {
    let date = name.strip_prefix("lios-")?.strip_suffix(".log")?;
    if date.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}
