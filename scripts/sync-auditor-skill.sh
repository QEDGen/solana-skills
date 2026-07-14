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
echo "synced qedgen-auditor to $destination"
