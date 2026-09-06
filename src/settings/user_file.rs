use super::{SettingsBudgets, SettingsLayer, SettingsPaths, SettingsProvider};
use std::path::Path;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: File =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(SettingsLayer {
        provider: file.provider,
        budgets: file.budgets,
        paths: file.paths,
    })
}
