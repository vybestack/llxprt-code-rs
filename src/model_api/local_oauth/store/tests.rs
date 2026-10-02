use super::*;
use std::os::unix::fs::symlink;

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn path(root: &tempfile::TempDir) -> std::path::PathBuf {
    root.path().canonicalize().unwrap()
}

#[test]
fn private_atomic_roundtrip_and_stable_lock() {
    let root = root();
    let path = path(&root);
    let store = LockedStore::open(&path).unwrap();
    assert_eq!(store.read().unwrap(), None);
    store.write(b"first").unwrap();
    let lock_inode = std::fs::metadata(path.join("oauth/codex.lock"))
        .unwrap()
        .ino();
    store.write(b"second").unwrap();
    assert_eq!(store.read().unwrap().unwrap(), b"second");
    assert_eq!(
        std::fs::metadata(path.join("oauth/codex.lock"))
            .unwrap()
            .ino(),
        lock_inode
    );
    assert_eq!(
        std::fs::metadata(path.join("oauth")).unwrap().mode() & 0o7777,
        0o700
    );
    for name in ["codex.lock", "codex.json"] {
        assert_eq!(
            std::fs::metadata(path.join("oauth").join(name))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(path.join("oauth")).unwrap().count(), 2);
}

#[test]
fn rejects_directory_and_root_symlinks() {
    let root = root();
    let victim = tempfile::tempdir().unwrap();
    let path = path(&root);
    symlink(victim.path(), path.join("oauth")).unwrap();
    assert!(LockedStore::open(&path).is_err());
    assert_eq!(std::fs::read_dir(victim.path()).unwrap().count(), 0);
    std::fs::remove_file(path.join("oauth")).unwrap();
    symlink(victim.path(), path.join("linked")).unwrap();
    assert!(LockedStore::open(&path.join("linked")).is_err());
}

#[test]
fn refuses_unsafe_directory_permissions_without_chmod() {
    let root = root();
    let path = path(&root);
    std::fs::create_dir(path.join("oauth")).unwrap();
    std::fs::set_permissions(path.join("oauth"), Permissions::from_mode(0o755)).unwrap();
    assert!(LockedStore::open(&path).is_err());
    assert_eq!(
        std::fs::metadata(path.join("oauth")).unwrap().mode() & 0o7777,
        0o755
    );
}

#[test]
fn refuses_symlink_hardlink_directory_fifo_and_public_token() {
    for kind in ["symlink", "hardlink", "directory", "fifo", "public"] {
        let root = root();
        let path = path(&root);
        let store = LockedStore::open(&path).unwrap();
        let token = path.join("oauth/codex.json");
        let victim = path.join("victim");
        std::fs::write(&victim, "secret-marker").unwrap();
        std::fs::set_permissions(&victim, Permissions::from_mode(0o600)).unwrap();
        match kind {
            "symlink" => symlink(&victim, &token).unwrap(),
            "hardlink" => std::fs::hard_link(&victim, &token).unwrap(),
            "directory" => std::fs::create_dir(&token).unwrap(),
            "fifo" => {
                use std::os::unix::ffi::OsStrExt as _;
                let name = CString::new(token.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "public" => {
                std::fs::write(&token, "token").unwrap();
                std::fs::set_permissions(&token, Permissions::from_mode(0o644)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(store.read().is_err(), "{kind}");
        assert!(store.write(b"replacement").is_err(), "{kind}");
        assert_eq!(std::fs::read(&victim).unwrap(), b"secret-marker");
    }
}

#[test]
fn unsafe_lock_is_not_followed_or_repaired() {
    let root = root();
    let path = path(&root);
    drop(LockedStore::open(&path).unwrap());
    let lock = path.join("oauth/codex.lock");
    std::fs::set_permissions(&lock, Permissions::from_mode(0o644)).unwrap();
    assert!(LockedStore::open(&path).is_err());
    std::fs::remove_file(&lock).unwrap();
    let victim = path.join("victim");
    std::fs::write(&victim, "preserved").unwrap();
    symlink(&victim, &lock).unwrap();
    assert!(LockedStore::open(&path).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"preserved");
}

#[test]
fn read_and_write_caps_and_failed_staging_preserve_previous() {
    let root = root();
    let path = path(&root);
    let store = LockedStore::open(&path).unwrap();
    let bytes = vec![b'x'; MAX_BYTES];
    store.write(&bytes).unwrap();
    assert_eq!(store.read().unwrap().unwrap(), bytes);
    assert!(store.write(&vec![b'x'; MAX_BYTES + 1]).is_err());
    let temp = path
        .join("oauth")
        .join(format!(".codex.{}.tmp", std::process::id()));
    std::fs::write(&temp, "stale").unwrap();
    assert!(store.write(b"new").is_err());
    assert_eq!(store.read().unwrap().unwrap(), bytes);
    std::fs::write(path.join("oauth/codex.json"), vec![b'x'; MAX_BYTES + 1]).unwrap();
    assert!(store.read().is_err());
}

#[test]
fn lock_wait_is_bounded_and_releases_on_drop() {
    let root = root();
    let path = path(&root);
    let store = LockedStore::open(&path).unwrap();
    let dir = openat::Dir::open(&path.join("oauth")).unwrap();
    let contender = open_file(&dir, LOCK, libc::O_RDWR, 0).unwrap();
    assert!(acquire(&contender, Duration::from_millis(1))
        .unwrap_err()
        .to_string()
        .contains("busy"));
    drop(store);
    acquire(&contender, Duration::from_secs(1)).unwrap();
}

#[test]
fn child_updates() {
    let Some(path) = std::env::var_os("LLXPRT_TEST_OAUTH_STORE") else {
        return;
    };
    for _ in 0..5 {
        let store = LockedStore::open(Path::new(&path)).unwrap();
        let count: u32 = String::from_utf8(store.read().unwrap().unwrap())
            .unwrap()
            .parse()
            .unwrap();
        store.write((count + 1).to_string().as_bytes()).unwrap();
    }
}

#[test]
fn multiple_processes_serialize_complete_updates() {
    let root = root();
    let path = path(&root);
    LockedStore::open(&path).unwrap().write(b"0").unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "model_api::local_oauth::store::tests::child_updates",
                ])
                .env("LLXPRT_TEST_OAUTH_STORE", &path)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        LockedStore::open(&path).unwrap().read().unwrap().unwrap(),
        b"20"
    );
}
