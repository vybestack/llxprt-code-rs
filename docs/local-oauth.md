# Filesystem OAuth storage

`--localoauth` explicitly selects filesystem storage for Codex OAuth credentials. Without this flag, the existing platform credential source remains selected. There is no automatic storage fallback, Keychain import, or migration. Selecting local storage does not read or modify Keychain records.

Sign in with a fresh device authorization:

```sh
llxprt-code-rs --localoauth --oauth-login
```

The command prints the authorization URL and device code to stderr. Complete authorization in the browser; the command stores the resulting token and emits one JSON result on stdout. Login does not consume a prompt or resolve a model profile. Device authorization has a 15-minute bound; individual HTTP requests have a 30-second bound. Error responses expose status only, never response bodies or tokens. The protocol uses the same client ID and endpoints as the TypeScript Codex device flow in `packages/auth/src/flows/codex-device-flow.ts`.

Use the selected storage on every worker invocation:

```sh
llxprt-code-rs --localoauth --profile-load /absolute/path/gpt-sol-high-rs.json \
  --session work-1 --allow-shell --prompt 'Your work request'
```

The token lives at `<configuration root>/oauth/codex.json`. Configuration-root resolution is shared with profiles, including `LLXPRT_CONFIG_HOME`. The `oauth` directory is owned by the current user with mode `0700`; token and lock files are single-link regular files owned by that user with mode `0600`. Symlinks in the configuration path or store, FIFOs, directories used as files, hard-linked files and unsafe permissions are rejected. Use a real absolute configuration path, not a symlink. Existing unsafe files are never chmod-repaired or overwritten automatically. Token reads and OAuth response bodies are bounded to 65,536 bytes.

## Concurrent instances

All cooperating processes use the stable `oauth/codex.lock` file. The lock is independent of the token inode, so atomic replacement does not split synchronization. Lock acquisition has a 120-second bound and the OS releases it when the descriptor closes or the process exits. Do not remove or replace the lock file while workers are running.

A worker acquires the lock and re-reads the stored credential before use. If it expires within the existing 30-second skew, refresh holds that same lock through the token exchange, validation and publication. Waiters then re-read the published token rather than sending another refresh with the old refresh token. A failed refresh preserves the old bytes and returns an error; it does not start an interactive login or switch storage.

Device login releases the lock while waiting for browser authorization. On completion it re-acquires the lock and compares the exact pre-login bytes. If another instance refreshed or signed in meanwhile, the newer state is retained and the login returns a conflict instead of overwriting it.

Publication writes a create-only private staging file in the same directory, syncs the file, atomically renames it over the validated destination and syncs the directory. Readers cannot observe a partially written token. A crash can leave an unreferenced private staging file; it is not a credential source. A post-rename directory-sync failure is reported even though the new file may already be installed.

Tokens are loaded and, if needed, refreshed when a backend is constructed. This does not refresh an already constructed backend in the middle of a turn or replay a failed model request. Filesystem permissions protect against other users, not programs running as the same user. The advisory lock coordinates participating LLxprt instances; unrelated programs must not modify the store during use.
