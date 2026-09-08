/// Identifier for the on-disk settings schema. The schema is strict *inside* each
/// Rust-owned section and tolerant at the shared top level (see `user_file::File`).
pub const SETTINGS_SCHEMA_ID: &str = "https://llxprt.dev/schema/settings-v1";
