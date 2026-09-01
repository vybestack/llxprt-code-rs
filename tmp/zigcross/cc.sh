#!/bin/bash
args=()
for a in "$@"; do
  case "$a" in
    --target=x86_64-unknown-linux-gnu) a="--target=x86_64-linux-gnu" ;;
    -target=x86_64-unknown-linux-gnu) a="-target=x86_64-linux-gnu" ;;
  esac
  args+=("$a")
done
exec zig cc "${args[@]}"
