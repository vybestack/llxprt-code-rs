# Release provenance and verification

## SerdesAI inputs

The build retains the eleven crates.io archives used to reconstruct the patched SerdesAI tree.
`provenance/serdes-ai-0.2.6.json` records each crates.io checksum and download URL pattern. It also
records the upstream Git commit, tree, and license blob. These identifiers can be checked without
relying on this repository:

1. Download each archive from
   `https://crates.io/api/v1/crates/<crate>/0.2.6/download` and compare its SHA-256 with the JSON
   record and the crates.io index entry.
2. Resolve commit `20fc3077e77a38ccc6d0ab5763098e44138630b5` in
   `https://github.com/janfeddersen-wq/serdesAI`. Its tree is
   `b03d19344e22181b6fc4bafe22c6cae344994519`.
3. Read `LICENSE` at that commit. Its Git blob is
   `fe61a2b80c84e4ff08965e9e952b02ee2be6a1f5` and its SHA-256 is
   `6854fea6c63a116a0cb7754cd9a6fea9c0578a64c50e850d87bef14579c6abf6`.
4. Run `python3 scripts/verify-upstream-evidence.py`, then reconstruct `vendor/` by following
   `PATCHES.md` and run `bash scripts/verify-vendor-provenance.sh`.

The offline verifier proves that the retained archives, embedded Cargo VCS metadata, and license
bytes match the recorded identities. It cannot prove that crates.io or GitHub supplied those
identities. Independent verification uses the HTTPS endpoints and Git repositories above. The
upstream commit is unsigned according to GitHub's Git commit API, so no upstream signature is
claimed.

## Tag and publication policy

A release tag must be an annotated `v<package-version>` tag that points directly to the workflow
commit. The package version and artifact names are derived from locked Cargo metadata. Before
publication, the workflow peels the remote tag through GitHub's Git-data API, verifies the local
source bundle and checksum, and attests both files.

GitHub's REST API has no compare-and-set operation that can publish a draft only if its asset list is
unchanged. The publisher therefore never creates or resumes a draft and never uploads release
assets. After checking remote policy, it creates a public release with deterministic metadata and an
empty asset list in one `POST`. The release body records the source-bundle name, SHA-256, and tagged
workflow run containing the attested artifact. This removes the unchecked draft-to-public window,
but it means the custom source bundle is a workflow artifact rather than a GitHub Release asset.
Workflow-artifact retention is a hosting limitation; the bundle remains reproducible from the tag.

The repository must enable immutable releases. It must also have an active, no-bypass ruleset that
applies to the exact release tag (or all refs) and prohibits tag updates and deletion. The publisher
checks both settings before release creation and rechecks the annotated tag afterward. Its token
therefore needs repository administration-read and contents-write permissions. No remote is
configured in the current local repository, so those settings and remote publication have not been
observed here.

Tag workflow runs do not cancel earlier runs. A tag-specific publication concurrency group permits
one publisher at a time. Any pre-existing release causes GitHub's create operation to fail; the
publisher never edits attacker-controlled title, body, prerelease, discussion, latest, target, or
asset state.

## Signed release attestation

The tagged publication job requests GitHub OIDC and attestation permissions and invokes the
commit-pinned `actions/attest-build-provenance` action for the exact archive and checksum sidecar.
GitHub binds the Sigstore attestation to the repository, workflow, workflow commit, and subject
hashes. Publication proceeds only if attestation succeeds.

The archive contains the upstream evidence JSON, all 11 retained archives, the complete patch, the
vendor verifier, the reviewed workflow, and the pinned action identity. Its generated content
manifest records each regular-file SHA-256. The attested archive subject therefore binds those
inputs and their exact bytes to the workflow commit; the sidecar gives an independently downloadable
copy of the archive digest. The attestation does not add an upstream signature or vouch for GitHub or
crates.io beyond the recorded identities and checks described above.

After a remote tagged run, record the attestation URLs from the workflow and verify both subjects:

```sh
gh attestation verify dist/llxprt-code-rs-<version>-source.tar.gz --repo <owner>/<repo>
gh attestation verify dist/llxprt-code-rs-<version>-source.tar.gz.sha256 --repo <owner>/<repo>
```

Also peel the annotated tag through the GitHub API, compare it with the attested workflow commit,
download both attested workflow artifacts, run `sha256sum --check`, and compare their names with the
version derived by `scripts/release-version.py`. Confirm that the immutable release has the exact
recorded title/body/tag/target, is neither draft nor prerelease, has no discussion, and has an empty
asset list. The action configuration is present, but no attestation or signature exists until a
tagged GitHub workflow completes successfully. A local annotated Git tag is not a cryptographic
signature unless a trusted signing key is separately configured and the signature is verified.
