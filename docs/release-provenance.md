# Release provenance and verification

## Release command ownership (issue #27)

Release orchestration and bundle policy run in the offline, locked xtask crate. The ten
release/test shell surfaces retain only current-interface entrypoints. CI invokes
`cargo +1.88.0 xtask publish-release` directly. No wrapper dispatches back into itself.

```sh
cargo +1.88.0 xtask source-bundle list
cargo +1.88.0 xtask source-bundle build [OUT.tar.gz]
cargo +1.88.0 xtask source-bundle verify [BUNDLE.tar.gz]
cargo +1.88.0 xtask source-bundle test
cargo +1.88.0 xtask test-release-workflow
cargo +1.88.0 xtask test-provider-features
cargo +1.88.0 xtask test-dependency-inventory
cargo +1.88.0 xtask verify-vendor-provenance
cargo +1.88.0 xtask test-vendor-provenance
cargo +1.88.0 xtask test-issue1-operator-protocol-runner
cargo +1.88.0 xtask run-issue1-operator-protocol interop
```

`--root ROOT` before the command selects the checked source tree for copied-tree fixtures;
it does not change the publisher's caller-relative `dist/`. Fixtures invoke the already-built
xtask executable, so a deliberately failing Cargo peer cannot accidentally bypass the builder
or verifier by intercepting wrapper startup. Python fixture suites live in `xtask/fixtures/`.
The existing Python archive parser, Git blob materializer, output policy, descriptor publisher,
and OCI helpers are unchanged.

The bundle member set remains the captured commit plus generated member and content manifests.
Rust enforces regular Git blob modes, deny rules, the load-bearing floor, registry scratch
rejection, and paired vendor provenance files. Standalone verification never runs extracted
code. It validates a bounded private snapshot, extracts it, and compares member names and
content against the checked source tree. Only the builder's explicit local-source verification
runs shipped offline gates, with an empty Cargo home, blackholed external proxies, and targets
outside the extraction. The publisher still retains the destination ancestor before construction:
`READY`, `PREPARE\0`, `PARENT_READY`, then the candidate pathname frame. Its existing timeout,
process-group cancellation, descriptor ownership, and no-replace installation remain active.

The operator runner keeps its explicit authorization, macOS requirement, external watchdog,
clean-checkout requirement, interop cleanup, exact single-test proof, capture cap, marker whitelist,
prerequisite ordering, and smoke retry budget. Its fixture uses a miniature checkout and external
sibling evidence directories under the test temporary root. It does not access a real keychain.

`release-gates` retains the existing ordered phases and heartbeat records. All Cargo gates use
Rust 1.88.0 with `--offline --locked`; Python and the existing system archive, patch, digest, Git,
and GitHub CLI dependencies remain required. Set `TMPDIR` to an isolated scratch directory for
fixture runs. No release is published by the fixtures.


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
assets. Before release creation, the workflow publishes the archive, checksum sidecar, and a
manifest that binds both files to the release tag and commit in `ghcr.io/<owner>/<repo>-source`.
A preflight check refuses to replace a commit-qualified tag observed with different content. OCI
Distribution has no portable atomic create-only tag operation, so the tag is mutable discovery
metadata rather than an identity boundary. Same-ref workflow concurrency serializes normal
publication runs. The publisher retrieves the manifest, config, archive, and sidecar anonymously by
digest and verifies their bytes; these digest-qualified objects are authoritative. Only after that
succeeds does it create a public release with deterministic metadata and an empty asset list in one
`POST`. The release body records the archive SHA-256 and digest-qualified URLs for the manifest and
both files.
The workflow artifact transfers files between jobs and may expire; it is not the durable release
location.

A new GHCR package is private by default. On first publication, anonymous verification therefore
fails before release creation. A package administrator must make `<repo>-source` public in GitHub's
package settings and rerun the tag workflow. In a serialized workflow run, the exact-content retry path
accepts the existing manifest without issuing another discovery-tag update. The tag remains mutable
registry metadata and is not used in release identity. Public GHCR packages have no default automatic
expiry, and this repository configures no cleanup workflow. OCI digests cannot be
reassigned to different bytes. A package administrator can still delete a package or version; GHCR
offers no repository setting that removes that administrative capability. This residual hosting
limitation applies to the durable objects even though release metadata references their digests.

The repository must enable immutable releases. It must also have an active, no-bypass ruleset that
applies to the exact release tag (or all refs) and prohibits tag updates and deletion. The publisher
requires GitHub to return an explicit empty `bypass_actors` array for the detailed ruleset; omitted,
null, malformed, inherited-but-hidden, and nonempty bypass data fail closed. A supplied
`current_user_can_bypass` must be `never`. The publisher checks both settings before release creation
and rechecks the annotated tag afterward. Its token therefore needs repository administration-read
and contents-write permissions. No remote is
configured in the current local repository, so those settings and remote publication have not been
observed here.

Tag workflow runs do not cancel earlier runs. A tag-specific publication concurrency group permits
one publisher at a time. Any pre-existing release causes GitHub's create operation to fail; the
publisher never edits attacker-controlled title, body, prerelease, discussion, latest, target, or
asset state.

## Signed release attestation

The tagged publication job requests GitHub OIDC and attestation permissions and invokes the
commit-pinned `actions/attest-build-provenance` action for the exact archive and checksum sidecar.
It separately attests the published OCI manifest digest and pushes that attestation to the registry.
GitHub binds the Sigstore attestations to the repository, workflow, workflow commit, and subject
hashes. Release creation proceeds only if both attestation steps succeed.

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

Also peel the annotated tag through the GitHub API and compare it with the attested workflow commit.
Fetch the OCI manifest and both blobs through the digest-qualified URLs in the release body. Check the
manifest digest, run `sha256sum --check`, and compare the names with the version derived by
`scripts/release-version.py`. Verify the manifest attestation against
`ghcr.io/<owner>/<repo>-source@sha256:<manifest-digest>`. Confirm that the immutable release has the
exact recorded title/body/tag/target, is neither draft nor prerelease, has no discussion, and has an
empty asset list. The action configuration is present, but no attestation or signature exists until a
tagged GitHub workflow completes successfully. A local annotated Git tag is not a cryptographic
signature unless a trusted signing key is separately configured and the signature is verified.
