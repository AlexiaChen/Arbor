#!/usr/bin/env bash
set -euo pipefail

status=0
while IFS= read -r file; do
  while IFS= read -r link; do
    target=${link%%#*}
    [[ -z "$target" || "$target" =~ ^https?:// ]] && continue
    resolved=$(dirname "$file")/$target
    if [[ ! -e "$resolved" ]]; then
      echo "missing documentation target in $file: $target" >&2
      status=1
    fi
  done < <(grep -oE '\]\(([^)#]+)(#[^)]+)?\)' "$file" \
    | sed -E 's/^\]\(([^)]+)\)$/\1/' \
    | sort -u)
done < <(find README.md AGENTS.md LEARNINGS.md doc -type f -name '*.md' | sort)
exit "$status"
