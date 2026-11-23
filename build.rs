use embed_manifest::{embed_manifest, manifest::ExecutionLevel, new_manifest};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Release builds demand administrator rights, which process tracing needs. Debug builds
    // don't, so `cargo run` and `cargo test` work from a normal terminal; start that terminal
    // as administrator to exercise tracing in a debug build.
    let level = if std::env::var("PROFILE").as_deref() == Ok("release") {
        ExecutionLevel::RequireAdministrator
    } else {
        ExecutionLevel::AsInvoker
    };
    embed_manifest(new_manifest("hdrify").requested_execution_level(level)).expect("failed to embed manifest");

    // The exe runs elevated: load imported DLLs from System32 only, never from the exe's folder.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
