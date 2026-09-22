use super::{SettingsBudgets, SettingsLayer, SettingsPaths, SettingsProvider};
use std::path::Path;

/// The settings file is a small typed config; larger files are a config error
/// before any parse, mirroring the profile file cap.
const MAX_SETTINGS_FILE_BYTES: usize = 4096;

/// The on-disk root of `settings.json`.
///
/// That root is a **shared, multi-tool surface**: the TypeScript llxprt-code app writes
/// its own keys into the same file (`ui`, `oauthEnabledProviders`,
/// `providerKeyfiles`, ...), so unknown **top-level** siblings are ignored here instead
/// of failing every launch (issue 202). Tolerance lives on this root struct only: each
/// section the Rust resolver owns (`provider`, `budgets`, `paths`) keeps its own
/// `deny_unknown_fields`, so a misspelled owned key, a wrong type, an invalid value, or
/// a repeated owned key still fails the settings load. There is no fallback reader, no
/// migration, and no write-back of sibling keys.
#[derive(serde::Deserialize)]
struct File {
    #[serde(default)]
    provider: SettingsProvider,
    #[serde(default)]
    budgets: SettingsBudgets,
    #[serde(default)]
    paths: SettingsPaths,
}

pub fn load_user_file(root: &Path) -> Result<SettingsLayer, String> {
    let path = root.join("settings.json");
    if !path.exists() {
        return Ok(SettingsLayer::default());
    }
    // Same guarded read as profile/keyfile loads: a special file (FIFO, device,
    // socket) must fail fast instead of blocking the open.
    use std::io::Read as _;
    let file = crate::safe_file::open_regular_nofollow(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut buf = Vec::with_capacity(MAX_SETTINGS_FILE_BYTES + 1);
    file.take((MAX_SETTINGS_FILE_BYTES as u64) + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if buf.len() > MAX_SETTINGS_FILE_BYTES {
        return Err(format!(
            "read {}: exceeds {} bytes",
            path.display(),
            MAX_SETTINGS_FILE_BYTES
        ));
    }
    let raw = String::from_utf8(buf).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: File =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(SettingsLayer {
        provider: file.provider,
        budgets: file.budgets,
        paths: file.paths,
    })
}
