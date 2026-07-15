#!/usr/bin/env bash
set -euo pipefail

forbidden='(^|[-_])(mpvss|pvss|pow|template|leveldb)([-_]|$)'
if cargo metadata --format-version 1 --no-deps \
  | tr ',{}' '\n' \
  | grep -E '"name":' \
  | grep -Ei "$forbidden"; then
  echo "forbidden default workspace dependency detected" >&2
  exit 1
fi

