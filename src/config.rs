//! Owns configuration-home resolution (env override order plus platform default)
//! and the ConfigHomeRoot newtype used by the session store and the provider
//! subsystem. This module is a leaf: it must NOT import anything from other crate
//! modules.

use std::path::{Path, PathBuf};

/// The platform config directory used by llxprt-code for profiles.
///
/// `LLXPRT_CONFIG_HOME` wins, then the legacy `LLXPRT_CONFIG_DIR` alias, then the
/// platform default: Windows `%AppData%\llxprt-code`, macOS
/// `~/Library/Preferences/llxprt-code`, otherwise `$XDG_CONFIG_HOME/llxprt-code`
/// falling back to `~/.config/llxprt-code`.
pub fn std_profile_dir() -> Result<PathBuf, String> {
    for name in ["LLXPRT_CONFIG_HOME", "LLXPRT_CONFIG_DIR"] {
        if let Some(path) = absolute_env_path(name)? {
            return Ok(path);
        }
    }
    let designed_dir = if cfg!(target_os = "windows") {
        absolute_env_path("AppData")?.map(|path| path.join("llxprt-code"))
    } else if cfg!(target_os = "macos") {
        home_dir()?.map(|path| path.join("Library/Preferences/llxprt-code"))
    } else if let Some(path) = absolute_env_path("XDG_CONFIG_HOME")? {
        Some(path.join("llxprt-code"))
    } else {
        home_dir()?.map(|path| path.join(".config/llxprt-code"))
    };
    designed_dir.ok_or_else(|| "absolute configuration directory is unavailable".to_string())
}

fn absolute_env_path(name: &str) -> Result<Option<PathBuf>, String> {
    std::env::var_os(name)
        .map(|value| require_absolute_path(name, PathBuf::from(value)))
        .transpose()
}

fn require_absolute_path(name: &str, path: PathBuf) -> Result<PathBuf, String> {
    (!path.as_os_str().is_empty() && path.is_absolute())
        .then_some(path)
        .ok_or_else(|| format!("{name} must name a nonempty absolute directory"))
}

fn home_dir() -> Result<Option<PathBuf>, String> {
    absolute_env_path("HOME")?
        .map_or_else(|| absolute_env_path("USERPROFILE"), |path| Ok(Some(path)))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigHomeRoot(PathBuf);

impl ConfigHomeRoot {
    pub(crate) fn discover() -> Result<Self, String> {
        Self::from_path(std_profile_dir()?)
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err("configuration home must be a nonempty absolute path".to_string());
        }
        Ok(Self(path))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf) -> Result<Self, String> {
        Self::from_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_selectors_require_nonempty_absolute_paths() {
        assert!(require_absolute_path("TEST_CONFIG", PathBuf::new()).is_err());
        assert!(require_absolute_path("TEST_CONFIG", PathBuf::from("relative")).is_err());
        assert_eq!(
            require_absolute_path("TEST_CONFIG", PathBuf::from("/absolute/config")).unwrap(),
            PathBuf::from("/absolute/config")
        );
    }
}
