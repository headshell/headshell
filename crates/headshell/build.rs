// The window's commands as the app's ACL manifest (D-086): each becomes an
// `allow-<name>` permission, which `main.rs` grants to the page's own
// address. The list is `src/command_names.rs`, shared with `main.rs`.
include!("src/command_names.rs");

fn main() {
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    if let Err(err) = tauri_build::try_build(attributes) {
        // A build script reports by failing: the message is the build's error.
        println!("cargo::error=STEP: BUILD\n  tauri-build failed: {err:#}");
        std::process::exit(1);
    }
}
