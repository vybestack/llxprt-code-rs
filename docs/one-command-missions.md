# Explicit one-command missions (issue #89)

A headless mission beginning with `Run exactly one shell command:` followed by a
single-line, backtick-delimited command is a single-dispatch contract. The prefix
is case-insensitive and may follow leading whitespace. The command bytes inside
the backticks are preserved, not trimmed, expanded, or rewritten by the agent.
Other prompts use the normal coding-agent loop; commands in examples or history
are not inferred as new missions. A malformed explicit mission fails closed.

For example:

```text
Run exactly one shell command: `python3 tmp/finisher.py`. It must print DETACHED.
Then stop immediately. Do not read files and do not call another tool.
```

The agent advertises only `run_shell_command` for this mission. It still asks the
configured backend for its first completion, so the model may decline execution;
the runtime does not execute arbitrary prompt text without a model tool request.
Before dispatch, the runtime requires exactly one shell call with a `command`
argument identical to the mission command. A read, another tool, a rewritten
command, a multi-call batch, or a no-tool refusal produces a typed
`one-command-refused` error and exit **6**, without executing any tool. The terminal
verdict is in `error.terminal_outcome`. A mixed batch is refused in full even if its
first call matches and the ordinary tool budget would permit that prefix.

The exact call uses the existing workspace capability, shell authorization,
timeout policy, tool/output limits, transcript admission, and session persistence.
Shell permission is still required. No inspection of ignored scripts or CTXDIGEST
handles is necessary, and this contract grants no additional permissions. CLI
budget validation remains unchanged (`--max-tool-calls 0` is a usage error).

After the call, there is **no follow-up model request**, summary request, or command
retry. A successful call completes with its admitted live tool result as the
summary and `tool_calls: 1`. Nonzero shell exit, timeout, or a shell-argument
failure produces `one-command-failed` and exit **6**, retaining the failed call and
its bounded diagnostics in the session. Infrastructure errors retain their
existing typed classifications. A failed mission remains failed when reopened;
normal explicit retry semantics apply, not a silent replay as success.

## Evidence

`tests/one_command_mission.rs` exercises the stock compiled CLI against a scripted
loopback HTTP provider. It verifies the advertised roster, actual request count,
terminal envelope, ignored-script side effect, and durable session records. It
covers a prohibited read, exact execution, mixed batch, modified command,
plain-text refusal, absent shell permission, malformed mission, nonzero exit,
timeout, unchanged CLI budget validation, and an ordinary-turn control.

At base `fac7c33cf23d0e7d7896a8bfbc64b0a13f1af8e8`, the initial five CLI regressions
failed with exit 101: the prohibited read spent a tool call, the mixed batch ran
the shell prefix, and the successful command was followed by a model summary
that claimed refusal. The fixed loop passes these invariants. Parser unit tests
also pin the narrow recognition boundary. These deterministic tests demonstrate
runtime enforcement, not live-model willingness to execute a particular script.

Review cycle/result: skipped per Andrew directive 2026-10-09 (no OCR).
