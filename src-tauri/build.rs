mod build_support;

fn main() {
    tauri_build::build();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").ok();
    let out_dir = std::env::var("OUT_DIR").expect("Cargo must set OUT_DIR");
    if let Some(archive) =
        build_support::resource_archive_link_arg(target_os.as_deref(), out_dir.as_ref())
    {
        println!("cargo:rustc-link-arg={archive}");
    }
}
