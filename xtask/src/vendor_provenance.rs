//! Checksum-pinned vendor reconstruction and documentation gate.
use crate::release_support::{checked, digest, python, Result, Temp};
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};

const ARCHIVES: &[(&str, &str)] = &[
    (
        "serdes-ai",
        "62dcf7d035a43aab94b8fed2925faa6f845d49de27066b2c9b07e339b3048a85",
    ),
    (
        "serdes-ai-agent",
        "95fd65311bcd469934e9cf5b4d10b6296fd9bde944aa2e232b0fedd37cca4aee",
    ),
    (
        "serdes-ai-core",
        "8c75900724c512454172492ffdd9ae24f8ccc5569e812c258a79d4151cd8934c",
    ),
    (
        "serdes-ai-macros",
        "8bd2f1e7f4f1f9a0a9f8b31ea0bb24b13271dd46817c8b656821701d1e1d4a40",
    ),
    (
        "serdes-ai-models",
        "cbca6da3265b8d1fce6255c4aee81b02ac9d2dba6e93829e09eaf1bc29d2886e",
    ),
    (
        "serdes-ai-output",
        "7c73a180c99d702c59282057d6f993332c8150834017110051f56e272133c54f",
    ),
    (
        "serdes-ai-providers",
        "8d857c9fc39b9c370eb7321fecb253c07a7892a3646c7455968a123da6df5a1d",
    ),
    (
        "serdes-ai-retries",
        "ebf2449d534d7ce2df7d743e61de516df945384aa50024965246ef5dfc638b93",
    ),
    (
        "serdes-ai-streaming",
        "159b5dfda85e1a886793e0962c6d40581044bb3ca008665b53f75ecb62eb3f74",
    ),
    (
        "serdes-ai-tools",
        "ae4c635d97827560acaa8d3af32a78fc50fece538d1e4638c889c7588f490777",
    ),
    (
        "serdes-ai-toolsets",
        "85e7ab76a1546ce6aa858c7a0fd438dd4235b3927fcf5a907bec26bacb6f2588",
    ),
];

pub fn run(root: &Path) -> Result {
    checked(&mut python(root, "verify-upstream-evidence.py"))?;
    checked(&mut python(root, "verify-serdes-responses-evidence.py"))?;
    let patch = root.join("SERDES-AI-0.2.6.patch");
    if digest(&patch)? != "0144b4e99ac63adf0daf17985a6e3fdb53d6c59f08c36c03b06308d519c3f660" {
        return Err("retained SerdesAI patch digest mismatch".into());
    }
    documentation(root)?;
    let stage = Temp::new(root, "llxprt-vendor-provenance")?;
    let vendor = stage.0.join("vendor");
    fs::create_dir(&vendor).map_err(|e| e.to_string())?;
    for (name, expected) in ARCHIVES {
        let archive = root.join(format!("vendor-upstream/{name}-0.2.6.crate"));
        if !fs::symlink_metadata(&archive).is_ok_and(|m| m.is_file()) {
            return Err(format!(
                "missing regular upstream archive: {}",
                archive.display()
            ));
        }
        if digest(&archive)? != *expected {
            return Err(format!(
                "upstream archive digest mismatch: {}",
                archive.display()
            ));
        }
        checked(
            Command::new("tar")
                .arg("-xzf")
                .arg(archive)
                .arg("-C")
                .arg(&stage.0),
        )?;
        let extracted = stage.0.join(format!("{name}-0.2.6"));
        if !fs::symlink_metadata(&extracted).is_ok_and(|m| m.is_dir()) {
            return Err("upstream archive did not produce the expected crate root".into());
        }
        fs::rename(extracted, vendor.join(name)).map_err(|e| e.to_string())?;
    }
    checked(Command::new("tar").arg("-xzf").arg(root.join("vendor-upstream/serdes-ai-responses-bd6aefc96f699276afb6384257b101039a663b5f.tar.gz")).arg("-C").arg(&vendor))?;
    if fs::read_dir(&stage.0).map_err(|e| e.to_string())?.count() != 1 {
        return Err("upstream archive produced an unexpected top-level path".into());
    }
    checked(
        Command::new("patch")
            .args([
                "--batch",
                "--forward",
                "--remove-empty-files",
                "--directory",
            ])
            .arg(&stage.0)
            .arg("-p1")
            .stdin(File::open(patch).map_err(|e| e.to_string())?)
            .stdout(Stdio::null()),
    )?;
    checked(
        Command::new("find")
            .arg(&vendor)
            .args(["-depth", "-type", "d", "-empty", "-delete"]),
    )?;
    checked(
        Command::new("diff")
            .args(["-ru", "--"])
            .arg(&vendor)
            .arg(root.join("vendor")),
    )
    .map_err(|e| {
        format!("vendored SerdesAI tree differs from checksum-pinned archives plus patch: {e}")
    })?;
    println!("vendor provenance checks ok");
    Ok(())
}

fn documentation(root: &Path) -> Result {
    let docs = fs::read_to_string(root.join("PATCHES.md")).map_err(|e| e.to_string())?;
    for required in [
        "patch --batch --forward --remove-empty-files -p1 < SERDES-AI-0.2.6.patch",
        "find vendor -depth -type d -empty -delete",
    ] {
        if !docs.contains(required) {
            return Err(format!("PATCHES.md does not document {required}"));
        }
    }
    let response = fs::read_to_string(root.join("vendor/serdes-ai-models/src/response.rs"))
        .map_err(|e| e.to_string())?;
    for required in [
        "async fn read_bounded",
        "pub(crate) const MAX_SUCCESS_BODY_BYTES: usize = 64 * 1024 * 1024;",
        "pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;",
        "Ok(\"provider returned an error response\".to_string())",
    ] {
        if !response.contains(required) {
            return Err(
                "PATCHES.md response limits differ from the retained response reader".into(),
            );
        }
    }
    for required in [
        "(`vendor/serdes-ai-models/src/response.rs`, `read_bounded`)",
        "`MAX_SUCCESS_BODY_BYTES = 64 * 1024 * 1024`",
        "`MAX_ERROR_BODY_BYTES = 64 * 1024`",
        "fixed, value-free diagnostic `provider returned an error response`",
    ] {
        if !docs.contains(required) {
            return Err(
                "PATCHES.md response limits differ from the retained response reader".into(),
            );
        }
    }
    Ok(())
}
