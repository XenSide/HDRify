fn main() {
    #[cfg(target_os = "windows")]
    {
        use embed_manifest::{embed_manifest, manifest::ExecutionLevel, new_manifest};
        embed_manifest(
            new_manifest("hdrify")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("failed to embed manifest");
    }
}
