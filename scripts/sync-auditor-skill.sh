#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_root="$repo_root/skills/qedgen-auditor"
destination="${1:-}"

if [[ -z "$destination" ]]; then
  echo "usage: sync-auditor-skill.sh <venue-owned-skill-destination>" >&2
  echo "error: destination is required; no harness-specific home is assumed" >&2
  exit 2
fi

mkdir -p "$destination"
rsync -a --delete "$source_root/" "$destination/"
if commit="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null)"; then
  printf '%s\n' "$commit" > "$destination/SOURCE_COMMIT"
fi
echo "synced qedgen-auditor to $destination"
