#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec "$repo_root/skills/qedgen-auditor/scripts/check-domain-artifacts.sh" "$@"
