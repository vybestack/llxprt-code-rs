//! Source bundle construction and verification. Archive parsing and descriptor-bound
//! publication remain in the existing Python helpers; policy and orchestration live here.
use crate::bundle_policy::{self as policy, DIGESTS, MANIFEST};
use crate::release_support::{cargo, checked, output, python, text, Result, Temp};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub fn run(root: &Path, args: &[String]) -> Result {
    match args.split_first() {
        Some((op, rest)) if op == "build" => build(root, rest),
        Some((op, rest)) if op == "verify" => verify(root, rest),
        Some((op, rest)) if op == "list" && rest.is_empty() => {
            print!("{}", policy::manifest(&policy::members(root, &policy::commit(root)?)?));
            Ok(())
        }
        _ => Err("usage: cargo xtask source-bundle <build [OUT]|list|verify [--run-local-source-code] [BUNDLE]|test>".into()),
    }
}

fn archive_argument(root: &Path, args: &[String]) -> Result<PathBuf> {
    let name = match args {
        [] => format!(
            "dist/{}",
            text(python(root, "release-version.py").args(["--value", "archive"]))?.trim()
        ),
        [name] if !name.starts_with("--") => name.clone(),
        _ => return Err("expected at most one archive pathname".into()),
    };
    Ok(root.join(name))
}

struct Publisher(Child);
impl Drop for Publisher {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = Command::new("kill")
                .args(["-TERM", &self.0.id().to_string()])
                .status();
            // The publisher owns bounded verifier-group termination and reaping.
            let _ = self.0.wait();
        }
    }
}

fn publisher(root: &Path, destination: &str) -> Result<(Publisher, UnixStream)> {
    let (reader, writer) = UnixStream::pair().map_err(|e| e.to_string())?;
    reader
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let descriptor: OwnedFd = writer.into();
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = python(root, "source-bundle-publish.py")
        .args(["--await-source", destination, "1", "--"])
        .arg(executable)
        .arg("--root")
        .arg(root)
        .args([
            "source-bundle",
            "verify",
            "--run-local-source-code",
            "{SOURCE}",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(descriptor))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok((Publisher(child), reader))
}

fn handshake(reader: &mut BufReader<UnixStream>, expected: &str) -> Result {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("publisher handshake: {e}"))?;
    if line == expected {
        Ok(())
    } else {
        Err(format!(
            "publisher handshake expected {expected:?}, received {line:?}"
        ))
    }
}

fn build(root: &Path, args: &[String]) -> Result {
    if args == ["--list"] {
        return run(root, &["list".into()]);
    }
    let _cancellation = crate::release_cancellation::Cancellation::install()?;
    let commit = policy::commit(root)?;
    let out = archive_argument(root, args)?;
    let destination = text(python(root, "source-bundle-output.py").arg(root).arg(&out))?;
    let destination = destination.trim_end_matches('\n');
    let (mut publisher, reader) = publisher(root, destination)?;
    let mut reader = BufReader::new(reader);
    handshake(&mut reader, "READY\n")?;
    let mut source = publisher.0.stdin.take().ok_or("publisher stdin missing")?;
    source.write_all(b"PREPARE\0").map_err(|e| e.to_string())?;
    handshake(&mut reader, "PARENT_READY\n")?;
    let stage = Temp::new(root, "llxprt-bundle-build")?;
    let archive = Temp::new(root, "llxprt-source")?;
    let files = policy::members(root, &commit)?;
    policy::validate(&files)?;
    checked(&mut python(root, "verify-registry-vendor.py"))?;
    hygiene(root, &files)?;
    let tracked = stage.0.join("tracked");
    fs::write(&tracked, files.join("\n") + "\n").map_err(|e| e.to_string())?;
    checked(
        Command::new("bash")
            .current_dir(root)
            .arg("scripts/verify-source-inputs-git.sh")
            .arg(root)
            .arg(&commit)
            .stdin(File::open(&tracked).map_err(|e| e.to_string())?),
    )?;
    let bundle = stage.0.join("bundle");
    checked(
        python(root, "materialize-git-tree.py")
            .arg(root)
            .arg(&commit)
            .arg(&bundle),
    )?;
    let listing = policy::manifest(&files);
    fs::write(bundle.join(MANIFEST), &listing).map_err(|e| e.to_string())?;
    digests(root, &bundle, &listing, &bundle.join(DIGESTS))?;
    let candidate = archive.0.join(".llxprt-source.candidate");
    make_tar(&stage.0, &candidate)?;
    drop(stage);
    source
        .write_all(candidate.as_os_str().as_encoded_bytes())
        .and_then(|()| source.write_all(b"\0"))
        .map_err(|e| e.to_string())?;
    drop(source);
    // Relay verifier output after the two protocol records. Do not apply the handshake timeout
    // to offline builds, which are bounded by the existing publisher-owned deadline.
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| e.to_string())?;
    use std::io::Read;
    let mut buffer = [0; 8192];
    loop {
        crate::release_cancellation::check()?;
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => std::io::stdout()
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let status = publisher.0.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("source-bundle publisher: {status}"));
    }
    println!("built and verified {destination}");
    Ok(())
}

fn hygiene(root: &Path, files: &[String]) -> Result {
    let roots: BTreeSet<_> = files
        .iter()
        .filter_map(|s| s.split_once('/').map(|(top, _)| top))
        .collect();
    for top in roots {
        walk_hygiene(&root.join(top), top, top == "registry-vendor")?;
    }
    Ok(())
}

fn walk_hygiene(path: &Path, relative: &str, registry: bool) -> Result {
    let name = path
        .file_name()
        .ok_or("missing path name")?
        .to_string_lossy();
    if matches!(
        name.as_ref(),
        ".git" | "target" | "dist" | "llxprt-parity-out" | "__pycache__"
    ) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.is_file() && matches!(name.as_ref(), ".cargo-ok" | ".rustc_info.json") {
        return Err(format!(
            "cargo-vendor scratch files are not permitted in source-bundle inputs: {relative}"
        ));
    }
    if !registry {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "symlinks are not permitted in source-bundle inputs: {relative}"
            ));
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(format!(
                "special files are not permitted in source-bundle inputs: {relative}"
            ));
        }
        if relative.bytes().any(|b| matches!(b, 9 | 10 | 13 | 127)) {
            return Err("control characters are not permitted in source-bundle paths".into());
        }
        if metadata.is_file() && policy::forbidden(relative) {
            return Err(format!(
                "forbidden path present in source directory: {relative}"
            ));
        }
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            walk_hygiene(
                &entry.path(),
                &format!("{relative}/{}", entry.file_name().to_string_lossy()),
                registry,
            )?;
        }
    }
    Ok(())
}

fn make_tar(stage: &Path, candidate: &Path) -> Result {
    let gnu = text(Command::new("tar").arg("--version"))?.contains("GNU");
    let mut command = Command::new("tar");
    command.arg("-C").arg(stage).env("COPYFILE_DISABLE", "1");
    if gnu {
        command.args([
            "--format=pax",
            "--sort=name",
            "--numeric-owner",
            "--owner=0",
            "--group=0",
            "--mode=u+rwX,go+rX,go-w",
            "--mtime=2021-01-01 00:00:00 UTC",
            "--pax-option=exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime",
        ]);
    }
    let mut tar = command
        .args(["-cf", "-", "bundle"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = checked(
        Command::new("gzip")
            .args(["-n", "-c"])
            .stdin(tar.stdout.take().ok_or("tar stdout missing")?)
            .stdout(File::create(candidate).map_err(|e| e.to_string())?),
    );
    let status = tar.wait().map_err(|e| e.to_string())?;
    result?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tar: {status}"))
    }
}

const DIGEST_PROGRAM: &str = r#"
import hashlib, os, pathlib, stat, sys
root, listing, destination = map(pathlib.Path, sys.argv[1:])
excluded = {'THIRD_PARTY_LICENSES/source-bundle.sha256', 'THIRD_PARTY_LICENSES/source-bundle.txt'}
with destination.open('w', encoding='ascii', newline='\n') as output:
    for name in sorted(listing.read_text().splitlines(), key=os.fsencode):
        if name.endswith('/') or name in excluded: continue
        path = root / name
        if not stat.S_ISREG(path.lstat().st_mode):
            raise SystemExit(f'trusted source member is not a regular file: {name}')
        output.write(f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {name}\n')
"#;

fn digests(root: &Path, source: &Path, listing: &str, destination: &Path) -> Result {
    let temp = Temp::new(root, "llxprt-digests")?;
    let list = temp.0.join("members");
    fs::write(&list, listing).map_err(|e| e.to_string())?;
    checked(
        Command::new("python3")
            .args(["-c", DIGEST_PROGRAM])
            .arg(source)
            .arg(list)
            .arg(destination),
    )
}

const SNAPSHOT: &str = r#"
import os, stat, sys
source, destination = sys.argv[1:]
inherited = os.environ.get('LLXPRT_BUNDLE_SOURCE_FD')
fd = os.dup(int(inherited)) if inherited else os.open(source, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
if not stat.S_ISREG(os.fstat(fd).st_mode): raise SystemExit('source bundle is not a regular file')
os.lseek(fd, 0, os.SEEK_SET)
with os.fdopen(fd, 'rb') as source, open(destination, 'xb') as target:
    remaining = 128 * 1024 * 1024
    while remaining:
        chunk = source.read(min(1024 * 1024, remaining))
        if not chunk: break
        target.write(chunk)
        remaining -= len(chunk)
    if source.read(1): raise SystemExit('source bundle exceeds the 134217728-byte compressed-size cap')
"#;

fn verify(root: &Path, args: &[String]) -> Result {
    let local = args.first().is_some_and(|a| a == "--run-local-source-code");
    let bundle = archive_argument(root, if local { &args[1..] } else { args })?;
    let stage = Temp::new(root, "llxprt-bundle-verify")?;
    let snapshot = Temp::new(root, "llxprt-bundle-snapshot")?;
    let candidate = snapshot.0.join("candidate.tar.gz");
    checked(
        Command::new("python3")
            .args(["-c", SNAPSHOT])
            .arg(bundle)
            .arg(&candidate),
    )?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o400))
        .map_err(|e| e.to_string())?;
    checked(python(root, "source-bundle-validate.py").arg(&candidate))?;
    checked(
        Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(&candidate)
            .arg("-C")
            .arg(&stage.0)
            .env("TAR_OPTIONS", "")
            .env("TAPE", ""),
    )?;
    let extracted = stage.0.join("bundle");
    if fs::read_dir(&stage.0).map_err(|e| e.to_string())?.count() != 1
        || !fs::symlink_metadata(&extracted)
            .map_err(|e| e.to_string())?
            .is_dir()
    {
        return Err("bundle root is not exactly one real bundle/ directory".into());
    }
    let actual = extracted_members(&extracted, "")?;
    for member in &actual {
        if policy::forbidden(member) {
            return Err(format!(
                "forbidden path present in extracted bundle: {member}"
            ));
        }
    }
    for required in REQUIRED {
        if !fs::symlink_metadata(extracted.join(required)).is_ok_and(|m| m.is_file()) {
            return Err(format!(
                "required regular file missing in extracted bundle: {required}"
            ));
        }
    }
    let trusted = policy::manifest(&policy::members(root, &policy::commit(root)?)?);
    equal_bytes(
        trusted.as_bytes(),
        &extracted.join(MANIFEST),
        "embedded manifest does not match the verifier's trusted source member list",
    )?;
    let trusted_digests = stage.0.join("digests.trusted");
    digests(root, root, &trusted, &trusted_digests)?;
    let expected = fs::read(&trusted_digests).map_err(|e| e.to_string())?;
    equal_bytes(
        &expected,
        &extracted.join(DIGESTS),
        "bundle content does not match the verifier's checked source tree",
    )?;
    let extracted_digests = stage.0.join("digests.extracted");
    digests(root, &extracted, &trusted, &extracted_digests)?;
    equal_bytes(
        &expected,
        &extracted_digests,
        "bundle content does not match the verifier's checked source tree",
    )?;
    if actual.join("\n") + "\n" != trusted {
        return Err(
            "extracted files and directories do not match the canonical member list".into(),
        );
    }
    if local {
        offline_gates(root, &extracted, &stage.0)?;
    } else {
        println!("source bundle structure and extracted member set verified; archive code was not executed");
    }
    Ok(())
}

fn equal_bytes(expected: &[u8], path: &Path, message: &str) -> Result {
    if fs::read(path).map_err(|e| e.to_string())? == expected {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn extracted_members(root: &Path, prefix: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = format!(
            "{prefix}{}",
            entry.file_name().to_str().ok_or("non-UTF-8 source name")?
        );
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            result.push(format!("{name}/"));
            result.extend(extracted_members(&entry.path(), &format!("{name}/"))?);
        } else if kind.is_file() {
            result.push(name);
        } else {
            return Err("extraction contains non-regular member".into());
        }
    }
    result.sort();
    Ok(result)
}

const REQUIRED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "LICENSE",
    "README.md",
    "PATCHES.md",
    "SERDES-AI-0.2.6.patch",
    ".gitignore",
    "src/lib.rs",
    "src/bin/llxprt-parity.rs",
    ".cargo/config.toml",
    "xtask/Cargo.toml",
    "xtask/Cargo.lock",
    "xtask/src/main.rs",
    "xtask/src/lib.rs",
    "xtask/src/release.rs",
    "vendor/serdes-ai/Cargo.toml",
    "vendor/serdes-ai/.cargo_vcs_info.json",
    "vendor/serdes-ai/src/lib.rs",
    "vendor/serdes-ai-core/Cargo.toml",
    "vendor/serdes-ai-core/src/lib.rs",
    "vendor/serdes-ai-models/Cargo.toml",
    "vendor/serdes-ai-models/src/openai/chat.rs",
    "vendor/serdes-ai-models/Cargo.lock",
    "vendor/serdes-ai-responses/Cargo.toml",
    "vendor/serdes-ai-responses/src/client/mod.rs",
    "provenance/serdes-ai-responses-git.json",
    "scripts/verify-serdes-responses-evidence.py",
    "vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz",
    "THIRD_PARTY_LICENSES/README.md",
    "THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt",
    MANIFEST,
    DIGESTS,
    ".github/workflows/ci.yml",
];

fn offline_gates(root: &Path, extracted: &Path, stage: &Path) -> Result {
    checked(&mut python(extracted, "verify-registry-vendor.py"))?;
    checked(&mut python(
        extracted,
        "verify-serdes-responses-evidence.py",
    ))?;
    let home = Temp::new(root, "llxprt-bundle-cargo-home")?;
    // JSON string quoting is a subset of TOML basic string syntax for this physical path.
    let registry = output(
        Command::new("python3")
            .args(["-c", "import json,sys; print(json.dumps(sys.argv[1]))"])
            .arg(extracted.join("registry-vendor")),
    )?;
    fs::write(home.0.join("config.toml"), format!("[source.crates-io]\nreplace-with = \"vendored-sources\"\n[source.vendored-sources]\ndirectory = {}\n", String::from_utf8(registry.stdout).map_err(|e| e.to_string())?.trim())).map_err(|e| e.to_string())?;
    let environment = |command: &mut Command, target: &Path| {
        command
            .env("CARGO_HOME", &home.0)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_NET_OFFLINE", "true")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "127.0.0.1,localhost");
    };
    for args in [
        vec![
            "test",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
        ],
        vec!["xtask", "quality"],
        vec![
            "test",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
        ],
        vec![
            "build",
            "--offline",
            "--release",
            "--locked",
            "--workspace",
            "--all-features",
        ],
    ] {
        let mut command = cargo(extracted);
        command.args(args);
        environment(&mut command, &stage.join("root-target"));
        checked(&mut command)?;
    }
    let target = Temp::new(root, "llxprt-bundle-provider")?;
    let mut surfaces = Command::new("bash");
    surfaces
        .current_dir(extracted)
        .arg("scripts/test-vendor-feature-surfaces.sh");
    environment(&mut surfaces, &target.0);
    checked(&mut surfaces)?;
    for (name, features) in [
        ("responses", vec![]),
        (
            "providers",
            vec!["--no-default-features", "--features", "openai"],
        ),
        ("models", vec!["--features", "openai"]),
    ] {
        let mut command = cargo(extracted);
        command
            .args(["test", "--offline", "--locked", "--manifest-path"])
            .arg(format!("vendor/serdes-ai-{name}/Cargo.toml"))
            .args(features);
        environment(&mut command, &target.0);
        checked(&mut command)?;
    }
    println!("bundle verify ok: single bundle/ top dir, exact member round-trip, tests and release build and direct provider tests pass");
    Ok(())
}
