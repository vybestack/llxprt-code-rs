use super::{SettingsBudgets, SettingsLayer, SettingsPaths, SettingsProvider};
use std::path::Path;

/// The settings file is a small typed config; larger files are a config error
/// before any parse, mirroring the profile file cap.
const MAX_SETTINGS_FILE_BYTES: usize = 4096;

#[derive(serde::Deserialize)]
// Shared multi-tool root: the rs resolver owns exactly `provider`, `budgets`,
// and `paths`. Foreign top-level keys (e.g. the TS app's `ui`,
// `oauthEnabledProviders`) are deliberately ignored here, so there is no
// `deny_unknown_fields` on this struct; the owned sub-objects below keep their
// own `deny_unknown_fields`, so an unknown key inside a namespace stays a hard
// error.
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
