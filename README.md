# llxprt-code-rs

A headless, **process-per-request** coding agent for llxprt profiles, written in Rust
and built on [`serdes-ai`] 0.2.6. Each invocation:

1. loads an llxprt profile (and its auth key) from the normal llxprt-code config,
2. drives **one turn** of a coding-agent loop: model tool calls are executed, results
   are fed back with matching call ids, and the read/edit/test cycle repeats until the
   model stops or the bounded turn limit is hit,
3. persists session/turn state under the real config dir,
4. prints **exactly one JSON object to stdout** (including on failure).

The project also ships a reusable library API ([`crate::cli`]), a black-box parity
harness ([`llxprt-parity`]), and scenario grading. The API key is only ever held
in memory; it is never logged or persisted.

> The equivalent implementation in TypeScript is the `llxprt-code` package in this
> workspace. `llxprt-code-rs` is the standalone Rust reboot. See
> [Known omissions vs llxprt-code](#known-omissions-vs-typescript-llxprt-code).

## Layout

```
src/
  main.rs        CLI binary: parse args, print the one JSON object, set exit code
  lib.rs         crate root
  cli.rs         args, profile/cwd/prompt resolution, turn resolution, public run()
  agent.rs       the turn loop: system prompt, tool-call execution, bounded cycle
  adapter.rs     SerdesAI wiring: OpenAI chat-completions request shape, tool schemas
  model.rs       ModelConfig resolution: profile key -> adapter settings (memory only)
  profile.rs     profile parsing + standard config-dir discovery
  session.rs     disk-backed session/turn store (flock + two-slot recovery, cwd pinning)
  tools.rs       tool specs and execution (file ops confined to --cwd), bounded shell
  harness.rs     black-box subprocess scaffolding shared by the parity binary
  grade.rs       structural / protocol / tool-use / build-test scoring
  bin/llxprt-parity.rs  the parity runner binary
tests/
  cli_contract.rs  offline black-box tests of the exactly-one-JSON contract
  phase2.rs        storage/branch/replay/lease/protocol tests via a mock backend
  process.rs       bounded-subprocess runner tests
```

## Architecture

```
  new process per request
  ┌─────────────────────────────────────────────────────────────┐
  │ llxprt-code-rs --session S --cwd W -p "prompt"  │
  │   cli::run                                          │
  │     profile  → model.rs → SerdesAI OpenAIChatModel    │
  │       base-url, model, {maxOutputTokens, sampling},  │
  │       auth-key (keyfile/settings.json; memory only)   │
  │     session store → resolve_turn, pin/verify --cwd   │
  │     agent.run(turn)                                 │
  │       ┌────────── tool loop (bounded) ──────────┐ │
  │       │ model → ToolCall(id, name, args)           │ │
  │       │   execute_tool (confined to cwd)          │ │
  │       │ append to session transcript                │  │
  │       │ feed assistant + ToolReturn(call_id) back  │  │
  │       └───────────────────────────────────────────────┘  │
  │   stdout: {session_id, turn, status, summary, …}    │
  └─────────────────────────────────────────────────────────────┘
```

Design constraints kept on purpose, and why:

- **One process, one turn.** State that must survive a process is persisted in two fixed,
  generation-numbered slots under `<config>/code-rs-sessions/<id>`; state that is per-request
  (the model transcript for this turn) lives in memory for the lifetime of that process. An
  interrupted slot write leaves the preceding valid generation available for recovery.
- **Exactly one JSON object on stdout.** Scripts and the parity harness parse it directly
  with no line sniffing. All human-facing logs go to stderr.
- **Fail fast.** A missing keyfile, empty base-url, bad profile, or unsupported
  provider returns a JSON error with a nonzero exit instead of a silent fallback.
- **cwd pinning.** The first turn binds the session to its `--cwd`; later turns with
  a different cwd are rejected (`cwd-mismatch`) so a replayed/branched turn can
  never drift into another directory. A brand-new session is pinned before any tool runs.

## Shell risk

The `run_shell_command` tool is **disabled by default**. When enabled with
`--allow-shell`, the model may run arbitrary shell commands with your user privileges in the
project `--cwd` (bounded output and timeout, nonzero exit returned to the model). This is
real code execution. Enable it only when you trust both the prompt and the model. The runner
terminates the process group it creates, but a child that deliberately calls `setsid` can detach
and outlive that group. This is not descendant containment or a sandbox. Use a container or VM
when the command or generated build is untrusted. The parity `dsflash` scenarios opt in
explicitly with `--allow-shell` because they must run real tests to be graded.

## Profile / key resolution

The config dir is discovered exactly like llxprt-code
([`crate::profile::std_profile_dir`]): the CLI is **Unix-only**. Discovery order:

1. `LLXPRT_CONFIG_HOME` if set (tests and scripts use this),
2. `LLXPRT_CONFIG_DIR` if set (legacy alias),
3. `~/Library/Preferences/llxprt-code` on macOS,
4. `$XDG_CONFIG_HOME/llxprt-code`, then `~/.config/llxprt-code` on Linux.

Default profile: `dsflash-mi300x` (`<config>/profiles/dsflash-mi300x.json`).
Override with `--profile NAME` or `--profile-load /path/to.json` (mutually exclusive).

Key precedence (matches llxprt-code):

1. `ephemeralSettings.auth-key` (inline),
2. `ephemeralSettings.auth-keyfile` (`~` is expanded),
3. `settings.json` → `providerKeyfiles[provider]` (openai family; `openaivercel`
   also falls back to `openai`).

`ephemeralSettings.auth-key-name` is a named **secure-store** reference, never a keyfile
path. This standalone binary has no compatible secure-store client, so a profile that
sets it fails fast during parsing with a fixed value-free refusal: the name is never
treated as a path and no filesystem access is ever attempted for it.

A *file* profile (`--profile-load`) must carry its own `auth-key`/`auth-keyfile`; it
never falls back to ambient `settings.json` credentials. The resolved key lives only in
[`ModelConfig`] and is never logged or persisted.

## Insecure HTTP gate

The `dsflash` profiles use a **remote plaintext HTTP endpoint on purpose**. Any `http://`
base URL whose host is not loopback (`localhost`, `127.0.0.1`, `::1`) is rejected
unless you pass `--allow-insecure-http` explicitly. HTTPS (any host) and loopback HTTP
stay allowed. Provider clients ignore inherited `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
and lowercase equivalents, so an ambient proxy cannot reroute a credential-bearing request.
Without the opt-in the CLI fails fast with a `model-config` `insecure http` error rather
than sending your key over plaintext by accident.

## CLI examples

```bash
# One turn of a starter project, default profile (dsflash-mi300x), plaintext-HTTP opt-in.
llxprt-code-rs --session demo-1 --cwd /tmp/ws --allow-insecure-http \
  -p "create a tiny python module + test and run it"

# With the shell tool enabled (real code execution; see "Shell risk").
llxprt-code-rs --session demo-1 --cwd /tmp/ws --allow-insecure-http --allow-shell \
  -p "create a tiny python module and run its test"

# Pipe the prompt; everything on stdout is one JSON object.
printf 'make a README.md' | llxprt-code-rs --cwd /tmp/ws2 --allow-insecure-http

# Explicit profile and a named later turn (continues the same workspace).
llxprt-code-rs --profile dsflash-mi300x --session demo-1 --cwd /tmp/ws \
  --allow-insecure-http --turn 2 -p "now add a double() helper and re-run tests"

# Different profile file.
llxprt-code-rs --profile-load ./my-profile.json --cwd /tmp/ws \
  --allow-insecure-http -p "..."

# Errors are still one JSON object with a nonzero exit.
llxprt-code-rs --profile definitely-missing -p "hi"
# {"session_id":"default","status":"error","error":{"code":"profile-missing","message":…}}
```

Example success object:

```json
{"session_id":"demo-1","session_dir":"…/code-rs-sessions/demo-1","turn":1,
 "status":"ok","summary":"…","tool_calls":7,"prompt_digest":"bfee86137d1c2b3e"}
```

## `--session` and `--turn` semantics (documented current behavior)

- `--session ID` is a directory name under `<config>/code-rs-sessions/`. Turn numbers
  restart per session and are 1-based.
- No `--turn` runs the **next** turn: the first invocation of a session runs turn 1,
  the second runs turn 2, and each new turn materializes all prior turn history as context
  and **continues in the same workspace**.
- `--turn N` where `N == latest+1` runs that next turn. `N <= latest` **replays** that
  turn: a completed branch with the same prompt at that turn is served entirely from the
  persisted session with no model request. If you pass a **different** prompt for an
  earlier turn, that becomes a new branch with explicit parent lineage and a fresh `branch_id`
  and does not corrupt the other turns' transcripts. Turn 0 is a usage error; a gap above
  `latest+1` is rejected.
- The transcript stores each turn's `prompt`, FNV-1a `digest`, and observed tool entries in
  generation-numbered `session.json` and `session.alt.json` slots under a per-session `flock`.
  Writes use retained directory and file descriptors, file and directory syncs, and post-sync
  identity checks. Readers select the newest valid generation and can recover from one malformed
  slot. Replays read the persisted record; they never rewrite history.
- `--cwd` is pinned to the session on its first turn; subsequent turns on the same session
  with a different cwd are rejected. `-p`/`--prompt` sets the prompt; when it is
  omitted the entire stdin is the prompt.

## Tool subset

`read_file`, `write_file`, `replace`, `list_directory`, `search_file_content`, and
(only with `--allow-shell`) `run_shell_command`. File paths are resolved relative to
`--cwd`; writes create parents inside the root and paths (including symlinks) that escape
are rejected. Tool arguments are strictly typed (missing required, wrong type, unknown extra
fields all fail). `read_file` honors an exact bounded `limit` (at most the requested
bytes, with an explicit truncation marker) and an exact `offset`; `replace`
requires `old_string` to match exactly once (or an `expected` count that matches exactly)
and a replace on a file above the supported size cap is rejected **without any
mutation**. `search_file_content` walks descriptor-relative (no symlink follow) and is
**hard-capped**: recursion depth, entries visited, aggregate source bytes read, aggregate
result bytes, and result count each have a hard cap; when a cap fires the walk stops
immediately at that point and the result carries an explicit truncation note with the
reporting reason(s). `list_directory`
and `search_file_content` never follow symlinks and cap their items/bytes. Materialized
history, single model responses (both the agent's round and the transport's 64 MiB
provider body cap), prompts, stdin, session files, tool output, and the inventory are all
bounded. Shell commands run via `/bin/sh -c` in `cwd` with a bounded timeout and
bounded combined stdout+stderr (a signal or timeout is surfaced to the model); they run
with your user privileges and are **not** sandboxed, so they are not confined by the
tool layer. Nonzero exits are reported to the model so it can repair the code.

## Base URL, endpoint routes, and `top_k`

A profile base URL is accepted only when its path is empty, `/`, `/v1`, `/v1/`,
`/chat/completions`, or `/v1/chat/completions`. Any arbitrary path prefix is rejected
with an unsupported/invalid-endpoint `model-config` error before a request is made. A bare origin, `/v1`, and the already-full route map to
`/v1/chat/completions` (the origin keeps that route: `http://host:8080` →
`http://host:8080/v1/chat/completions`), and the redacted
`scheme://host:port` rendering is never sent as the request URL. Userinfo, query,
and fragment are rejected. The OpenAI chat-completions request has no `top_k` field, so
a profile that sets `top_k` is rejected up front as an unsupported setting rather
than silently dropped or forwarded.

## JSON output contract

- Exactly **one** JSON object on stdout, both on success and on error, for every
  invocation. stderr may carry logs; the parity harness and `cli_contract` tests assert
  this.
- Success: `session_id`, `session_dir`, `turn`, `attempt`, `branch_id`, `branch`,
  `replayed`, `status: "ok"`, `summary`, `tool_calls` (int), `prompt_digest`.
- Error: `session_id`, `status: "error"`, `error: {code, message}`, and a nonzero
  exit (2 usage, 3 config, 4 session, 5 model, 6 turn).
- `--help`/`--version` are the only stdout exceptions (Clap renders them and exits 0).
- A missing value for `-p` or an unknown flag is a usage error on the main CLI:
  strictly one JSON object and exit 2. Clap never prints raw help text for these.
  The main CLI has no scenario argument; `llxprt-parity` (a separate binary) is the
  command that takes `--scenarios` and treats an empty or unknown scenario allow-list as a
  usage error. Parity contract is covered under [Parity harness](#parity-harness); the
  JSON-on-stdout + nonzero-exit contract above applies to the main CLI.

`--param` is not a flag of this binary. Turn continues (a later turn without `--turn`,
or `--turn N`) call the model again against the persisted session; `--branch BRANCH`
starts a new branch. Replays happen only when the same prompt is re-run against the same
turn in the same lineage. `publish = false` in `Cargo.toml`: the vendored
patched `serdes-ai` path dependency cannot be published to crates.io. `cargo
package` is **not** a release gate; the release build is of this source tree in place
with the `vendor/` path deps (the vendored `serdes-ai` tree is required to build
and is never excluded from the source). The SerdesAI MIT notice shipped with the vendored
distributions is recorded in `THIRD_PARTY_LICENSES/SERDES-AI-MIT.txt`.

## Reasoning (actual behavior)

The `reasoning.*` settings in the installed `dsflash*` profiles influence the *author's*
streaming renderer. This headless CLI sends non-streaming chat completions and does not set a
reasoning field; `reasoning.effort` is surfaced as a text note in the system prompt
when the profile uses it, and the reasoning note is added locally to the prompt text, never
forwarded as a transport setting. Request-side keys the openai path cannot apply are
rejected (`model-config unsupported profile behavior`) unless ignoring them would make the
`dsflash` family unusable, in which case they are accepted only for that family, retained as
local compatibility flags, and never forwarded: `shell-replacement`, `emojifilter`,
`stream-idle-timeout-ms`, `requires-auth`, `streamIdleTimeoutMs`, `maxRetrywait`,
`reasoning.maxTokens`, `reasoning.budgetTokens`, `autokimi-style`, `sandbox-base-url`,
`default-tools`, `tool-format`, `reasoning.enabled`, `reasoning.includeInResponse`,
`reasoning.includeInContext`, `reasoning.stripFromContext`, `reasoning.effortWireFormat`,
`reasoning.enabledWireFormat`, `reasoning.enabledMap`, `reasoning.effortMap`,
`reasoning.format`, `reasoning.fieldName`, `reasoning.update`, and `reasoning.display`.

## Scope and platform

`llxprt-code-rs` is **Unix-only** (macOS/Linux; the process runner uses
`setsid`/process groups). Interactive mode, emoji filters, hooks/permissions, telemetry,
token-usage logging, streaming, and the full llxprt-code tool catalog are not ported.
The session store and subprocess runner are Unix-only by design; there is no Windows support.

## cwd safety

Sessions are pinned to a working directory (`session.json["cwd"]`) on their first turn
and enforce it on every later turn (`cwd-mismatch`). File tools retain one descriptor-relative
workspace capability for the complete turn; path arguments cannot escape it through `..`,
absolute paths, symlinks, or a concurrent rename of the workspace path. `run_shell_command`
is separate host execution enabled only by `--allow-shell`. Its process-group supervision is
not a filesystem sandbox, and a process that deliberately detaches into a new session can
outlive that process group.

### Concurrency contract for `replace`

`write_file` and `replace` calls serialize on an advisory lock held through each write. The
lock is attached to the retained workspace directory, so cooperating LLxprt processes that opened
the same directory inode cannot interleave those operations. `replace` reads the target, derives
replacement bytes, then publishes them atomically with a temporary file and rename. Immediately
before the rename it re-opens the final name no-follow and verifies identity (`dev`/`ino`), file
type, size, and a SHA-256 digest against the bytes from which the replacement was derived. A
change detected by that check returns a conflict. Callers can also pass `expected_sha256`, an
independently computed lowercase hex SHA-256 of the complete current content, as an up-front
precondition.

The advisory lock only coordinates programs that honor it. The verification is not an atomic
compare-and-swap because the re-open/verify and rename are separate syscalls. An unrelated process
can change the name between those calls, so callers must not treat `replace` as protection against
uncoordinated external writers.

## Offline checks

```bash
cargo fmt --all -- --check
cargo fmt --all --manifest-path xtask/Cargo.toml -- --check
cargo test --offline --locked --manifest-path xtask/Cargo.toml
cargo clippy --offline --locked --manifest-path xtask/Cargo.toml --all-targets --all-features -- -D warnings
cargo xtask quality
cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings
cargo test --offline --locked --workspace --all-targets --all-features   # the full suite
cargo +1.88.0 check --offline --locked --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --offline --locked --workspace --all-features --no-deps
cargo audit --no-fetch               # works only when a local advisory cache exists
bash scripts/verify-vendor-licenses.sh
cargo build --offline --release --locked --workspace --all-features
bash scripts/build-source-bundle.sh
```

`cargo package` is explicitly **not** a release gate and is never run in CI: the crate is
`publish = false`, and the release artifact is the source tree built in place with its `vendor/`
path dependencies. The local `cargo` gates above use `--offline`; loopback provider tests never
contact an installed endpoint. In CI the audit job installs a pinned `cargo-audit` and runs
`cargo audit` with a normal network fetch. A local `cargo audit --no-fetch` only works
when a local advisory cache already exists (it is a local-only gate, never a CI command).

### Rust size and complexity limits

`cargo xtask quality` enforces four fixed production limits: 800 effective lines per file,
80 effective lines per function or method, cyclomatic complexity 25, and cognitive
complexity 30. `cargo xtask loc` and `cargo xtask complexity` run the two metric groups
separately. `cargo xtask lint` runs root formatting, strict Clippy, and all four metrics.
A value equal to a limit passes; a value above it fails.

The production scope is every Rust file under the root crate's `src/`, including
`src/bin/` and ordinary in-tree test modules under `src/`. The `tests/` integration directory,
vendored code, and the `xtask` implementation are outside that scope.
Blank and comment-only lines do not count toward effective LOC. Multiline code constructs
count each occupied code line once. Function spans include signatures, attributes, closing
braces, and nested closures. Free functions, implementation methods, default trait methods,
and nested functions are measured independently.

Cyclomatic complexity starts at 1 and counts `if`/`if let`, loops, match arms after the
first, match guards, and each `&&` or `||`. Cognitive complexity starts at 0. Structured
decisions add one plus their nesting depth; additional match arms, guards, boolean sequence
changes, and recognized direct recursion each add one. Direct recursion is recognized through
a bare name, `self::name`, `Self::name`, or `self.name()`. Nested functions are separate units;
closures remain part of their containing function. These metrics cover authored syntax in the
parsed source files, not compiler-generated code from derives or dependency macros. Local macro
definitions and `include!` fail closed because they could hide authored functions or control flow
outside that syntax tree. Renamed `include!`, path-selected modules, and macro invocations
containing explicit function or complexity-bearing syntax also fail closed.
The limits cannot be raised or bypassed through command-line options, baselines, allowlists,
per-file exceptions, or suppressions. Syntax, prohibited macro expansion, or source traversal
errors fail the gate.

## Source release bundle

Because there is no `cargo package` (see above), the distributable source artifact is a tarball
archived from one clean committed Git snapshot. The fixed allow-list is asserted in both directions
against that commit rather than used as a mutable-tree copy filter. Publication fails when `HEAD` is
absent, when tracked changes exist, or when the allow-listed member set differs from the committed tree.
Build and verify it with:

```bash
bash scripts/build-source-bundle.sh            # build dist/... then verify it
bash scripts/verify-source-bundle.sh         # verify the existing dist/... tarball
bash scripts/build-source-bundle.sh --list   # print the exact member list, no build
```

What it contains: the crate files (`Cargo.toml`, `Cargo.lock`,
`LICENSE`, `README.md`, `PATCHES.md`, `SERDES-AI-0.2.6.patch`, `.gitignore`),
the whole `src/`, `tests/`, `scripts/`, `.github/`, `THIRD_PARTY_LICENSES/`,
`vendor-upstream/`, and `registry-vendor/`, `.cargo/config.toml`, the xtask manifest, lockfile,
and source, and the required vendored serdes-ai crates'
`Cargo.toml`/`Cargo.toml.orig`/`README.md`/`.cargo_vcs_info.json`/`Cargo.lock`/`src`. Retaining
all vendor lockfiles permits byte-for-byte reconstruction from the upstream archives and patch;
the models lockfile is also required by its direct locked provider tests. The bundle includes the
generated member manifest `THIRD_PARTY_LICENSES/source-bundle.txt` and the per-file content
manifest `THIRD_PARTY_LICENSES/source-bundle.sha256`. The 11 retained
`vendor-upstream/*.crate` archives have the crates.io SHA-256 values recorded in `PATCHES.md`;
`scripts/verify-vendor-provenance.sh` applies the retained patch to fresh extractions and requires
an exact match with `vendor/`. It never contains `.git`,
`target/`, `dist/`, `llxprt-parity-out/`, logs, `.DS_Store`, or cargo-vendor
scratch, and the build fails if one is found where it would be bundled. The archive has a
single top-level `bundle/` directory. The builder rejects symlink inputs. An output must be outside
the physical source tree or a proper descendant of its physical `dist/` directory; `dist/` itself
is not a file destination. Verification snapshots the archive before
validation, validates all member names and types before extraction, rejects paths outside
`bundle/`, links, and special files, requires zero-size directory payloads, and enforces limits
of 128 MiB compressed input, 16 MiB per regular member, less than 384 MiB aggregate
regular-member bytes, and 448 MiB for the complete expanded tar stream, including concatenated
gzip members, headers, and metadata. It requires the byte-sorted embedded manifest to match the
trusted verifier tree's allow-list, generated from the same fixed build policy rather than from
archive-authored data, checks the extraction in both directions, and compares every regular source
file digest with the verifier's checked tree. Release verification must use
`verify-source-bundle.sh` from the matching reviewed tag so that this content comparison binds the
artifact to that tag; another revision may intentionally have different source bytes or policy.
Standalone verification stops there and does not execute archive-controlled code.
Before checking the committed source identity or constructing an archive, the builder starts the
publisher and waits until it has walked the physical output path no-follow and retained its deepest
existing directory. After the commit check, the publisher creates only components that were absent
during setup, pins the final output directory, and acknowledges it. It then waits on an inherited
anonymous pipe for the candidate pathname, opens and unlinks the candidate, and retains both
publication descriptors through explicit local-source verification. Rust 1.88 xtask tests and the production quality gate, strict root test/release gates,
and direct locked SerdesAI provider and model tests use private target directories. On success, the
publisher installs the retained, digest-checked bytes through a descriptor-bound, create-only
primitive. Linux copies into an anonymous `O_TMPFILE` and links that descriptor into the retained
output directory. macOS clones the retained, already-unlinked source descriptor with
`fclonefileat`, which requires clone-compatible source and destination filesystems and otherwise
fails before publication. A concurrent final-destination winner is never replaced. Source-name,
output-parent, and destination-directory substitution cannot redirect publication. The builder's
original writable pathname is never published.

The retained archives are also bound to crates.io checksums, upstream Git commit/tree identity, and
the upstream license Git blob by `provenance/serdes-ai-0.2.6.json`. See
[`docs/release-provenance.md`](docs/release-provenance.md) for independent verification, immutable
tag and release requirements, atomic release-record publication, and signed GitHub attestation
verification.

Before the one-shot zero-asset GitHub Release `POST`, the tagged workflow publishes the archive and
sidecar as OCI blobs in the public `ghcr.io/<owner>/<repo>-source` package. A deterministic OCI
manifest binds those digest-addressed files to the tag and commit. A preflight check refuses an
observed same-commit-tag collision. OCI Distribution has no portable create-only tag update, so the
commit-qualified tag is mutable discovery metadata; same-ref workflow serialization prevents normal
publication runs from racing, while digest-qualified references remain authoritative. Publication
anonymously retrieves the manifest, config, archive, and sidecar by digest and attests the files and
OCI manifest. The release body contains the stable digest URLs. A package's first GHCR
publication defaults private, so it must fail before release creation until an administrator makes
the package public; the exact-content rerun is safe. GHCR has no default automatic package expiry and
this repository has no cleanup automation. Package administrators retain the ability to delete
objects. See the provenance guide for bootstrap and verification details.

The bundle contains the complete crates.io source closure for all 13 retained lockfiles.
`scripts/verify-registry-vendor.py` checks the package inventory, package checksums, file inventory,
and every vendored file digest. Bundle verification runs with an empty temporary `CARGO_HOME`,
`--offline --locked`, unusable network proxies, and fresh target directories. The manifest is
written inside the staged bundle and the checked tree is never mutated, so repeated builds over the
same sources are repeatable. `bash -n` and ShellCheck on
`scripts/*.sh` are CI gates; on GNU tar the archive is byte-reproducible, on BSD/macOS
tar (no GNU ordering flags) it is well-formed but not byte-reproducible.

## Parity harness

`llxprt-parity` is a separate binary (`cargo run --bin llxprt-parity -- --help`).
It defaults to the `dsflash-mi300x` profile (use `--profile NAME` to override). For
each scenario it:

1. creates a fresh isolated workspace and a unique session id,
2. invokes the **real CLI as a subprocess** (`LLXPRT_CODE_RS_BIN` env var, or the
   `cargo test`-baked binary path) with `--allow-insecure-http` and `--allow-shell`
   passed explicitly,
3. parses strictly **one JSON object** per invocation (typed extraction, exit `0` checked),
4. preserves the raw stdout/stderr **bytes** verbatim (exactly as captured, possibly
   invalid UTF-8), with explicit per-stream truncation flags, plus the typed meta on
   disk,
5. aborts follow-ups after the first failed turn,
6. writes a JSON report and prints it, exiting nonzero if any requested scenario fails its
   grader (build, structural, protocol, or hidden-grader evidence, not protocol alone).

```bash
LLXPRT_CODE_RS_BIN=target/debug/llxprt-code-rs \
  cargo run --bin llxprt-parity -- --scenarios starter      # live smoke, default

LLXPRT_CODE_RS_BIN=target/debug/llxprt-code-rs \
  cargo run --bin llxprt-parity -- --all                   # starter, pong, flappy, encryption

LLXPRT_CODE_RS_BIN=target/debug/llxprt-code-rs \
  cargo run --bin llxprt-parity -- --scenarios pong,encryption --out /tmp/parity-out
```

The parity binary prints progress to stderr and exactly one final JSON object to stdout. It
publishes the report path create-only and prints that path only to stderr. A failure before
publication emits `error.code = "report-persist"` and exits 3. If the final file is already
visible but a later directory-sync or cleanup step fails, stdout instead reports
`report-published-durability-unconfirmed`, includes `published: true` and the published report,
and exits 3. The default path is `llxprt-parity-out/report-<run_id>.json`.

### Scenario grading

Four scenarios: `starter` (tiny Python module + test), `pong` (headless Pong core,
text runner, tests), `flappy` (headless Flappy core, ASCII runner, tests), and
`encryption` (a Rust library using an established crypto crate with roundtrip and
wrong-password/tamper tests). Each may use follow-up turns within a per-scenario budget.

The report is evidence, not claims:

- `scores.protocol` — every CLI invocation returned `"status":"ok"` with every required
  envelope field, the requested session and turn, and exit code 0.
- `scores.tool_use` — the CLI's validated per-turn `tool_calls` (the model's own
  executed tool-call count for the turn, from the typed envelope) are >= 2 total for a
  full score. The evidence never comes from scanning session branches.
- `scores.build_test` — the **grader re-runs** the real check in the produced
  workspace: `python3 test_pong.py`, `python3 test_flappy.py`, the starter python
  check, or `cargo test --offline` for encryption.
- `scores.structural` — the required files exist, each a real non-symlink regular file
  (`score_present`), and the file inventory is sorted and capped with a truncation flag.
- `hidden_graders_pass` — deterministic behavioral probes of the produced files (e.g. the
  starter's `math_utils.py` defines `add` and `test_math_utils.py` exercises
  `add(2,3)`). Hidden graders read no-follow and cap file size.

A scenario only passes when protocol, tool-use, build/test, structural, and hidden graders
all pass.

## Known omissions vs TypeScript llxprt-code

- **Interactive mode**, emoji filters, hooks/permissions system, `--param`, and the full
  llxprt-code tool catalog are not ported.
- **Streaming, telemetry, and token-usage logging** are absent; requests are
  non-streaming chat completions.
- The `context-limit` preflight uses a 3-bytes-per-token request-size heuristic. It is not the
  provider's tokenizer and cannot guarantee acceptance at the model's exact token boundary. The
  separate hard byte caps still bound request construction and transport.
- The parity harness preserves the CLI's raw stdout/stderr bytes on disk with an explicit
  truncation flag from the bounded runner; a captured stream larger than the cap is
  truncated, never spliced.
- `reasoning`, `top_k`, and the dsflash request-side profile flags are never forwarded
  over the wire (`top_k` is rejected as unsupported; the rest are prompt notes or
  documented-and-ignored).

## Sessions

- **Lifecycle and leases.** A reserved branch is `pending` under a unique owner token with a
  1-hour lease, renewed around every model request and checkpoint. A `pending` branch whose
  lease has expired can be reclaimed by another process; a live pending reservation by another
  owner is `busy`. Renewal, checkpoint, finalize, and fail all re-verify the owner token,
  prompt digest, and turn, so a stale owner can never mutate a reclaimed branch.
- **Grader semantics.** Protocol, tool-use, build/test, structural, and hidden-grader scores
  are independent behavioral graders: each is computed from its own re-run (or no-follow file
  probe), a scenario only passes when all pass, and a good score in one category cannot cover
  for a failure in another.

[`serdes-ai`]: https://crates.io/crates/serdes-ai
[`crate::cli`]: src/cli.rs
[`llxprt-parity`]: src/bin/llxprt-parity.rs
[`crate::profile::std_profile_dir`]: src/profile.rs
[ModelConfig]: src/model.rs
