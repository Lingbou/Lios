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

#[test]
fn windows_loader_is_only_bundled_on_windows() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shared = read_json(&manifest.join("tauri.conf.json"));
    let windows = read_json(&manifest.join("tauri.windows.conf.json"));

    assert!(shared["bundle"].get("resources").is_none());
    assert_eq!(
        windows["bundle"]["resources"]["bin/WebView2Loader.dll"],
        "WebView2Loader.dll"
    );
    assert!(manifest.join("bin/WebView2Loader.dll").is_file());
}

#[test]
fn desktop_binary_name_is_reserved_for_the_desktop_product() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = fs::read_to_string(manifest.join("Cargo.toml")).unwrap();
    let config = read_json(&manifest.join("tauri.conf.json"));

    assert!(cargo
        .lines()
        .any(|line| line.trim() == "name = \"lios-desktop\""));
    assert!(!cargo.lines().any(|line| line.trim() == "name = \"lios\""));
    assert_eq!(config["mainBinaryName"], "lios-desktop");
}

#[test]
fn windows_installer_is_current_user_only_and_does_not_modify_path() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shared = read_json(&manifest.join("tauri.conf.json"));
    let windows = read_json(&manifest.join("tauri.windows.conf.json"));
    let nsis = &windows["bundle"]["windows"]["nsis"];

    assert_eq!(shared["productName"], "Lios");
    assert_eq!(shared["bundle"]["publisher"], "Lingbou");
    assert_eq!(nsis["installMode"], "currentUser");
    assert!(nsis.get("installerHooks").is_none());
    assert!(!manifest.join("windows/nsis-hooks.nsh").exists());
    assert!(!manifest.join("windows/path-helper.ps1").exists());
}

#[test]
fn release_workflow_is_unsigned_and_checksum_only() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(
        manifest
            .parent()
            .unwrap()
            .join(".github/workflows/release.yml"),
    )
    .unwrap();

    for required in [
        "name: Unsigned release",
        "name: Prepare checksummed release assets",
        "sha256sum \"${payloads[@]}\" > SHA256SUMS",
        "sha256sum --check SHA256SUMS",
        "name: Publish unsigned GitHub Release",
        r#"$installDir = Join-Path $env:LOCALAPPDATA "Lios""#,
    ] {
        assert!(
            workflow.contains(required),
            "release workflow must contain {required:?}"
        );
    }

    for forbidden in [
        "${{ secrets.",
        "WINDOWS_CERTIFICATE",
        "Import-PfxCertificate",
        "TAURI_SIGNING_CONFIG",
        "RPM_SIGNING_KEY",
        "TAURI_SIGNING_RPM_KEY",
        "rpmsign",
        "gpg --batch",
        "cosign",
        "actions/attest@",
        "id-token: write",
        ".sigstore.json",
        "rpm-signing-public.asc",
        "certificateThumbprint",
        r#"/D=$installDir"#,
        "lios-nsis-path-smoke",
    ] {
        assert!(
            !workflow.contains(forbidden),
            "unsigned release workflow must not contain {forbidden:?}"
        );
    }
}
