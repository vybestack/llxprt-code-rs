use super::open_regular_at;

#[cfg(test)]
type PublicationCallback = Box<dyn FnOnce(&str)>;

#[cfg(test)]
thread_local! {
    static STAGE_SUBSTITUTION_HOOK: std::cell::RefCell<Option<PublicationCallback>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_stage_substitution_hook(callback: Option<PublicationCallback>) {
    STAGE_SUBSTITUTION_HOOK.with(|hook| hook.replace(callback));
}

#[cfg(test)]
fn run_stage_substitution_hook(name: &str) {
    STAGE_SUBSTITUTION_HOOK.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback(name);
        }
    });
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationHookPoint {
    AfterRename,
    BeforeDirectorySync,
    AfterDirectorySync,
}

#[cfg(test)]
type PublicationHook = (PublicationHookPoint, PublicationCallback);

#[cfg(test)]
thread_local! {
    static PUBLICATION_HOOK: std::cell::RefCell<Option<PublicationHook>> =
        const { std::cell::RefCell::new(None) };
    static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn install_publication_hook(
    point: PublicationHookPoint,
    callback: Box<dyn FnOnce(&str)>,
) {
    PUBLICATION_HOOK.with(|hook| hook.replace(Some((point, callback))));
}

#[cfg(test)]
pub(crate) fn fail_next_directory_sync() {
    FAIL_DIRECTORY_SYNC.with(|fail| fail.set(true));
}

#[cfg(test)]
fn run_publication_hook(point: PublicationHookPoint, leaf: &str) {
    PUBLICATION_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if hook
            .as_ref()
            .is_some_and(|(expected, _)| *expected == point)
        {
            let (_, callback) = hook.take().expect("publication hook disappeared");
            callback(leaf);
        }
    });
}

/// Atomically install `content` over `leaf`, authenticate the retained staging descriptor before
/// and after rename, sync the retained parent, and authenticate the installed bytes again.
pub(super) fn atomic_write_into(
    parent: &openat::Dir,
    leaf: &str,
    content: &[u8],
) -> Result<(), String> {
    atomic_write_into_after(parent, leaf, content, || Ok(()))
}

pub(super) fn atomic_write_into_after(
    parent: &openat::Dir,
    leaf: &str,
    content: &[u8],
    before_install: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut stage = RetainedStage::create(parent, leaf, content)?;
    before_install()?;
    #[cfg(test)]
    run_stage_substitution_hook(&stage.name);
    stage.verify_name(parent)?;
    parent
        .local_rename(&stage.name, leaf)
        .map_err(|error| format!("rename over {leaf}: {error}"))?;
    #[cfg(test)]
    run_publication_hook(PublicationHookPoint::AfterRename, leaf);

    stage
        .authenticate_destination(parent, leaf)
        .map_err(|error| installed_unknown(leaf, &error))?;
    #[cfg(test)]
    run_publication_hook(PublicationHookPoint::BeforeDirectorySync, leaf);
    stage
        .authenticate_destination(parent, leaf)
        .map_err(|error| installed_unknown(leaf, &error))?;
    sync_parent(parent).map_err(|error| installed_unknown(leaf, &error))?;
    #[cfg(test)]
    run_publication_hook(PublicationHookPoint::AfterDirectorySync, leaf);
    stage
        .authenticate_destination(parent, leaf)
        .map_err(|error| installed_unknown(leaf, &error))
}

struct RetainedStage {
    file: std::fs::File,
    name: String,
    dev: u64,
    ino: u64,
    len: u64,
    digest: [u8; 32],
    clear_on_drop: bool,
}

impl RetainedStage {
    fn create(parent: &openat::Dir, leaf: &str, content: &[u8]) -> Result<Self, String> {
        use std::io::Write as _;
        use std::os::unix::fs::MetadataExt as _;

        let name = random_temp_name()?;
        let mut file = parent
            .new_file(&name, 0o600)
            .map_err(|error| format!("create temp in {leaf}: {error}"))?;
        file.write_all(content)
            .map_err(|error| format!("write {leaf}: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync temp in {leaf}: {error}"))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            name,
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: content.len() as u64,
            digest: sha256_bytes(content),
            clear_on_drop: true,
        })
    }

    fn verify_name(&self, parent: &openat::Dir) -> Result<(), String> {
        if self.destination_has_identity(parent, &self.name)? {
            Ok(())
        } else {
            Err("staging file identity changed before publication".to_string())
        }
    }

    fn authenticate_destination(&mut self, parent: &openat::Dir, name: &str) -> Result<(), String> {
        match self.destination_has_identity(parent, name) {
            Ok(true) => {}
            Ok(false) => {
                self.clear_on_drop = true;
                return Err("installed file identity mismatch".to_string());
            }
            Err(error) => {
                self.clear_on_drop = true;
                return Err(error);
            }
        }
        if let Err(error) = self.verify_destination(parent, name) {
            self.clear_on_drop = true;
            return Err(error);
        }
        self.clear_on_drop = false;
        Ok(())
    }

    fn destination_has_identity(&self, parent: &openat::Dir, name: &str) -> Result<bool, String> {
        use std::os::unix::fs::MetadataExt as _;
        let file = open_regular_at(parent, name)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok(metadata.file_type().is_file()
            && metadata.dev() == self.dev
            && metadata.ino() == self.ino)
    }

    fn verify_destination(&self, parent: &openat::Dir, name: &str) -> Result<(), String> {
        use std::io::Read as _;
        let mut file = open_regular_at(parent, name)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() || metadata.len() != self.len {
            return Err("installed file type or length mismatch".to_string());
        }
        let mut bytes = Vec::with_capacity(self.len as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if sha256_bytes(&bytes) != self.digest {
            return Err("installed file digest mismatch".to_string());
        }
        Ok(())
    }
}

impl Drop for RetainedStage {
    fn drop(&mut self) {
        if self.clear_on_drop {
            let _ = self.file.set_len(0);
            let _ = self.file.sync_all();
        }
    }
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

fn sync_parent(parent: &openat::Dir) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_DIRECTORY_SYNC.with(|fail| fail.replace(false)) {
        return Err("injected retained-parent sync failure".to_string());
    }

    use std::os::fd::AsRawFd as _;
    if unsafe { libc::fsync(parent.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "sync retained parent directory: {}",
            std::io::Error::last_os_error()
        ))
    }
}

fn installed_unknown(leaf: &str, detail: &str) -> String {
    format!("installed {leaf}, but durability or integrity is unknown: {detail}")
}

fn random_temp_name() -> Result<String, String> {
    let mut random = [0u8; 16];
    fill_random(&mut random)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(".llxprt-tmp-{suffix}"))
}

#[cfg(target_os = "macos")]
fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    Ok(())
}

#[cfg(target_os = "linux")]
fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    let count = unsafe {
        libc::syscall(
            libc::SYS_getrandom,
            bytes.as_mut_ptr().cast::<libc::c_void>(),
            bytes.len(),
            0,
        )
    };
    if count == bytes.len() as libc::c_long {
        Ok(())
    } else {
        Err(format!(
            "generate private staging name: {}",
            std::io::Error::last_os_error()
        ))
    }
}
