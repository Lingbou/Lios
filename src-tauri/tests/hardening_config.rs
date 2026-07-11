use std::fs;
use std::path::Path;

use serde_json::Value;

const PRODUCTION_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; font-src 'self' data:; img-src 'self' asset: data: blob:; connect-src 'self' ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";
const DEV_CSP: &str = "default-src 'self'; script-src 'self' http://127.0.0.1:5173; style-src 'self' 'unsafe-inline' http://127.0.0.1:5173; font-src 'self' data:; img-src 'self' asset: data: blob: http://127.0.0.1:5173; connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:5173 ws://127.0.0.1:5173; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn production_csp_is_strict_and_dev_csp_only_relaxes_vite_hmr() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = read_json(&manifest.join("tauri.conf.json"));
    let security = &config["app"]["security"];

    assert_eq!(security["csp"], PRODUCTION_CSP);
    assert_eq!(security["devCsp"], DEV_CSP);
    let production = security["csp"].as_str().unwrap();
    assert!(!production.contains("'unsafe-inline'"));
    assert!(!production.contains("'unsafe-eval'"));
    assert!(!security["devCsp"]
        .as_str()
        .unwrap()
        .contains("'unsafe-eval'"));
}

#[test]
fn capabilities_allow_only_used_dialog_and_window_operations() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let capability = read_json(&manifest.join("capabilities/default.json"));
    let permissions = capability["permissions"].as_array().unwrap();
    let actual = permissions
        .iter()
        .map(|permission| permission.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            "core:default",
            "dialog:allow-open",
            "dialog:allow-save",
            "core:window:allow-minimize",
            "core:window:allow-toggle-maximize",
            "core:window:allow-close",
            "core:window:allow-start-dragging",
        ]
    );
}
