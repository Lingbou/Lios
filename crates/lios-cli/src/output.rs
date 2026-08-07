use serde::Serialize;
use serde_json::{json, Value};

use crate::error::CliError;

#[derive(Debug)]
pub struct CommandOutput {
    pub human: Vec<String>,
    pub result: Value,
}

impl CommandOutput {
    pub fn new(result: impl Serialize) -> Self {
        Self {
            human: Vec::new(),
            result: serde_json::to_value(result).unwrap_or(Value::Null),
        }
    }

    pub fn human(mut self, line: impl Into<String>) -> Self {
        self.human.push(line.into());
        self
    }
}

pub fn render_success(json_mode: bool, command: &str, output: CommandOutput) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "ok": true,
                "command": command,
                "result": output.result,
            }))
            .expect("JSON envelope serialization")
        );
    } else {
        for line in output.human {
            println!("{line}");
        }
    }
}

pub fn render_error(json_mode: bool, command: &str, error: &CliError) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "ok": false,
                "command": command,
                "error": error,
            }))
            .expect("JSON envelope serialization")
        );
    } else {
        eprintln!("lios: {error}");
    }
}
