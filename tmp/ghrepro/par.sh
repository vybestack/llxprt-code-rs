#!/usr/bin/env bash
# Two concurrent gh issue create calls from a non-git cwd, mimicking the
# broker's buildMinimalEnv host path. Exit codes land in par.exit.
cd /Users/acoliver/projects/llxprt/agent/main || exit 9
GH=/opt/homebrew/bin/gh
M="PATH=$PATH HOME=$HOME GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1"
env PATH="$PATH" HOME="$HOME" GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 \
  "$GH" issue create --repo vybestack/llxprt-code-rs \
  --title 'gh-broker repro D (parallel, will close)' \
  --body-file llxprt-code-rs/tmp/ghrepro/bodyb.txt \
  > llxprt-code-rs/tmp/ghrepro/d.out 2> llxprt-code-rs/tmp/ghrepro/d.err &
P1=$!
env PATH="$PATH" HOME="$HOME" GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 \
  "$GH" issue create --repo vybestack/llxprt-code-rs \
  --title 'gh-broker repro E (parallel, will close)' \
  --body-file llxprt-code-rs/tmp/ghrepro/bodyc.txt \
  > llxprt-code-rs/tmp/ghrepro/e.out 2> llxprt-code-rs/tmp/ghrepro/e.err &
P2=$!
wait "$P1"; D=$?
wait "$P2"; E=$?
printf 'D-EXIT=%d\nE-EXIT=%d\nD-err:[%s]\nE-err:[%s]\nD-out:[%s]\nE-out:[%s]\n' \
  "$D" "$E" \
  "$(cat llxprt-code-rs/tmp/ghrepro/d.err | cut -c1-200)" \
  "$(cat llxprt-code-rs/tmp/ghrepro/e.err | cut -c1-200)" \
  "$(cat llxprt-code-rs/tmp/ghrepro/d.out)" \
  "$(cat llxprt-code-rs/tmp/ghrepro/e.out)" \
  > llxprt-code-rs/tmp/ghrepro/par.exit
cat llxprt-code-rs/tmp/ghrepro/par.exit
