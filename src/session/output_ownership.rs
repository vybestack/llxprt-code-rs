//! A session namespace is wholly store-owned, including future publications.
//! Validate before creating diagnostic output; no file-name list or store mutation.
use crate::config::ConfigHomeRoot;
use std::io::{Error, ErrorKind, Result};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

fn identity(dir: &openat::Dir) -> Result<(u64, u64)> {
    let meta = dir.open_file(".")?.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

fn invalid() -> Error {
    ErrorKind::InvalidInput.into()
}

/// Output parents use the same strict root capability as the JSONL sink. Walk
/// retained descriptor ancestry, not the user spelling: intermediate aliases,
/// case aliases and `..` cannot hide membership. Keep this very parent for create.
pub(crate) fn validate_diagnostic_destination(
    config: &ConfigHomeRoot,
    path: &Path,
    parent: &openat::Dir,
) -> Result<()> {
    let config_dir = match crate::tools::open_root(config.as_path()) {
        Ok(dir) => dir,
        Err(_) => {
            // A normal fresh config is legal. Other failures are not absence:
            // metadata distinguishes permissions/types and final aliases.
            return match std::fs::symlink_metadata(config.as_path()) {
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                _ => Err(invalid()),
            };
        }
    };
    let config_identity = identity(&config_dir)?;
    let store = match config_dir.sub_dir("code-rs-sessions") {
        Ok(dir) => Some(identity(&dir)?),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(_) => return Err(invalid()),
    };
    // The namespace root itself is reserved even before it exists. Its parent
    // is identified by descriptor, including aliases. ASCII case variants are
    // reserved too so case-insensitive filesystems cannot publish over a sink.
    if identity(parent)? == config_identity
        && path.file_name().is_some_and(|name| {
            name.to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("code-rs-sessions"))
        })
    {
        return Err(invalid());
    }
    if let Some(store_identity) = store {
        let mut current = parent.sub_dir(".")?;
        // Directory ancestry reaches the root, bounded by the OS path depth.
        // A filesystem failing ancestry inspection is not accepted as outside.
        for _ in 0..4096 {
            let here = identity(&current)?;
            if here == store_identity {
                return Err(invalid());
            }
            let next = current.sub_dir("..")?;
            if identity(&next)? == here {
                return Ok(());
            }
            current = next;
        }
        return Err(invalid());
    }
    Ok(())
}
