# Native E investigation — not acceptance

Source inspected: `6ffa513eef9380debf77043e703fadcc560579a0` on branch4-issue252-parity-ownership. No production changes or new test-pass claims in this investigation. PR #290 remains draft.

## Platform constraint

The actual host is Darwin, uid 501. `darwin-ownership-capabilities.txt` preserves local SDK evidence. Existing `src/process.rs::kill_group` signals `-child.id()` and falls back to the direct child. `process_launch.rs::launch_coordinator` publishes only that group identifier. No descendant ownership mechanism was found in this path. This is insufficient for a descendant that calls setsid or moves process groups, independently of whether the direct child is retained unreaped.

Darwin's SDK explicitly documents NOTE_TRACK/NOTE_CHILD as unsupported since 10.5. A kqueue fork tracker cannot legitimately be used as portable macOS containment. Repeated PPID enumeration is also insufficient: a descendant can fork and reparent between observations. Checking start time before kill has a check/signal race. Neither is an acceptable implementation of the requested guarantee.

The SDK exposes audit-token signaling, which can improve identity-safe signaling of an already known process, but does not by itself discover all owned descendants. EndpointSecurity has kernel lifecycle events, but its client API requires entitlement, TCC approval, and root privileges; those capabilities are not supplied to this uid-501 workspace. No privileged installation or host configuration was attempted. A supported kernel ownership/containment facility for arbitrary escaping descendants on this host remains unresolved. This is a concrete platform-capability constraint, not evidence that the rest of the coding work is complete. Linux subreaper/cgroup approaches are not macOS implementations.

## Evidence and limitations

Fetched issue #252 and PR #290 bodies and their issue-comment payloads, plus PR reviews, into this directory. Source review was partial; full comment review and implementation review remain unfinished. Existing native-d logs were not changed. No Cargo gates were launched in this investigation; no lifecycle matrix, exact retained descendant identities, or successful full-suite exit is claimed. No unrelated jobs were signaled.

Remaining work includes locked fixture lifecycle, real external/nested scratch execution, cancellation/identity proof, all gates, and full source review. The earlier full-test timeout is a tool scheduling defect, not a passing test; future gates require an owned background supervisor with an exit marker and exact identity evidence. Read output digesting was worked around with small windows and is not a blocker.

Acceptance is **not complete**. In particular there is no claim that group cleanup now covers escaped descendants. Do not mark this PR ready or merge based on this investigation.
