# Inspecting an interrupted worker

For a worker killed during a tool call, start with:

```sh
llxprt-code-rs transcript --session TASK_ID --json
# Optional: --turn N; omit --json for readable output.
```

A SIGINT/SIGTERM during a live turn is represented by a typed cancellation
marker. JSON reports `turns[].lifecycle: "cancelled"` and:

```json
{
  "cancellation": {
    "signal": "SIGTERM",
    "in_flight": {
      "tool": "run_shell_command",
      "id": "call-id",
      "turn": 1,
      "round": 6,
      "started": 1792297924,
      "args": "{\"command\":\"cargo test --offline --locked\"}"
    }
  }
}
```

`turn` and `round` are one-based; `started` is Unix seconds. `args` is the
validated argument-object JSON text, scrubbed using the transcript's secret/URL
redaction and then UTF-8 bounded to **8192 bytes**, including `[truncated]` when
cut. It is diagnostic text, not an executable command or necessarily parseable
JSON after truncation. Redaction is not a guarantee that arbitrary shell text
contains no sensitive information: treat the session directory as private.

During execution, the same tool evidence is atomically published as
`<config-home>/code-rs-sessions/TASK_ID/tool-in-flight.json`. This is independent
of `--emit`; a worker killed even by SIGKILL leaves the last published call
available there. Ordinary completion (including a tool failure) removes this
watchdog sidecar. A turn interrupted between tools or during a provider call
has `cancellation.in_flight: null`, not the last completed tool.

The persistent cancellation evidence uses private, owner-scoped
`interruption-<sha256-of-reservation-owner>.signal` and `.tool.json` files beside
the session log. The signal file is empty until cancelled, then contains `I`
(SIGINT) or `T` (SIGTERM). The tool file contains the diagnostic object above,
or `null` once the call resolves; it may be absent before the first tool.
These files survive process exit. The transcript joins them to the matching
pending reservation, never another owner or a completed/failed branch. Preserve
the session directory **before retry/reclaim or further work**: reclaim replaces
the reservation owner, and a subsequent tool replaces the watchdog sidecar.
The owner-scoped files remain available for operator forensics after reclaim.

The handler does not acquire the session lock, serialize history, or invent a
completed tool result. It writes one byte to a pre-opened regular file, kills
the active tool process group as before, and exits **130** (SIGINT) or **143**
(SIGTERM). The transaction log can therefore still contain a pending lease;
`transcript`'s `cancelled` lifecycle is a read-only projection of the typed
marker, not a manufactured completed round. SIGKILL, machine power loss, or
storage write failures cannot produce a signal marker; an in-flight sidecar
alone is not proof of which signal or sender killed the worker.

Evidence setup/publication failures are typed session errors and prevent tool
dispatch rather than silently running without evidence. The signal handler's
write remains best-effort: a failing disk must never prevent tool-group cleanup
or change the signal exit status. This does not change shell permissions, hide
worker PIDs, or identify the process that sent a signal.

The new session artifacts advance the supported store format to **version 8**,
as required by the storage-codec guard. Older stores are rejected; use a fresh
session. No migration or legacy-format reader is provided.
