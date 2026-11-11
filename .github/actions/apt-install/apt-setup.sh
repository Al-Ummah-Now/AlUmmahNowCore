#!/usr/bin/env bash
# Leaves apt with a mirror that answers and a fresh package index, or exits 1.
#
# 2026-11-11: the runners' default mirror (azure.archive.ubuntu.com, through
# /etc/apt/apt-mirrors.txt) stopped resolving. apt-get update then "succeeded"
# with warnings ("Some index files failed to download") and every install either
# failed with exit 100 or retried the dead host for 15 minutes. Retrying the same
# mirror does not help: on a failed or partial update this switches the sources
# to the next mirror and tries again, each try time-boxed.
set -uo pipefail

printf 'Acquire::Retries "2";\nAcquire::http::Timeout "20";\nAcquire::https::Timeout "20";\n' \
  | sudo tee /etc/apt/apt.conf.d/80-ci-retries > /dev/null

# "" keeps whatever the sources say now (the runner's default, or a mirror an earlier call chose)
MIRRORS=("" "http://archive.ubuntu.com/ubuntu/" "http://us.archive.ubuntu.com/ubuntu/" "http://mirrors.edge.kernel.org/ubuntu/")
SOURCES=(/etc/apt/sources.list.d/ubuntu.sources /etc/apt/sources.list)
PATTERN='mirror\+file:/etc/apt/apt-mirrors\.txt|https?://([a-z]+\.)?archive\.ubuntu\.com/ubuntu/?|https?://security\.ubuntu\.com/ubuntu/?|https?://mirrors\.edge\.kernel\.org/ubuntu/?'

use_mirror() {
  local m=$1 f
  [ -z "$m" ] && return 0
  for f in "${SOURCES[@]}"; do
    [ -f "$f" ] && sudo sed -i -E "s#${PATTERN}#${m}#g" "$f"
  done
  echo "apt: switched sources to $m"
}

for m in "${MIRRORS[@]}"; do
  use_mirror "$m"
  out=$(timeout 180 sudo apt-get update 2>&1)
  code=$?
  if [ "$code" -eq 0 ] && ! grep -qE '^(W|E): (Failed to fetch|Some index files failed)' <<< "$out"; then
    echo "apt: package index ready${m:+ from $m}"
    exit 0
  fi
  echo "::warning::apt-get update failed${m:+ on $m} (exit $code): $(grep -E '^(W|E):' <<< "$out" | head -2 | tr '\n' ' ')"
done
echo "::error::no Ubuntu mirror answered"
exit 1
