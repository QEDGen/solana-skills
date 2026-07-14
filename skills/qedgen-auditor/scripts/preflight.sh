#!/usr/bin/env bash
set -euo pipefail

MIN_QEDGEN_VERSION="2.42.0"
skill_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
skill_version="$(tr -d '[:space:]' < "$skill_root/VERSION")"
root=""
spec=""
qedgen_bin="${QEDGEN_BIN:-qedgen}"
run_compile=0

usage() {
  echo "usage: preflight.sh --root <program-root> [--spec <path>] [--qedgen <binary>] [--compile]" >&2
}

version_at_least() {
  awk -v have="$1" -v need="$2" 'BEGIN {
    split(have, h, "."); split(need, n, ".");
    for (i = 1; i <= 3; i++) {
      hv = h[i] + 0; nv = n[i] + 0;
      if (hv > nv) exit 0;
      if (hv < nv) exit 1;
    }
    exit 0;
  }'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) root="${2:-}"; shift 2 ;;
    --spec) spec="${2:-}"; shift 2 ;;
    --qedgen) qedgen_bin="${2:-}"; shift 2 ;;
    --compile) run_compile=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$root" || ! -d "$root" ]]; then
  usage
  echo "error: --root must name an existing program directory" >&2
  exit 2
fi
root="$(cd "$root" && pwd -P)"

if [[ -n "$spec" ]]; then
  [[ "$spec" = /* ]] || spec="$root/$spec"
  if [[ ! -f "$spec" ]]; then
    echo "error: explicit spec does not exist: $spec" >&2
    exit 2
  fi
  spec="$(cd "$(dirname "$spec")" && pwd -P)/$(basename "$spec")"
else
  specs=()
  while IFS= read -r candidate; do
    [[ -n "$candidate" ]] && specs+=("$candidate")
  done < <(find "$root" -type f -name '*.qedspec' -not -path '*/target/*' -not -path '*/.git/*')
  if [[ ${#specs[@]} -eq 1 ]]; then
    spec="${specs[0]}"
  elif [[ ${#specs[@]} -gt 1 ]]; then
    echo "error: multiple .qedspec files found; pass --spec explicitly" >&2
    printf 'candidate_spec=%s\n' "${specs[@]}" >&2
    exit 2
  fi
fi

runtime_dep_regex='(^|[^[:alnum:]_-])(anchor-lang|pinocchio|quasar-lang|solana-program)([^[:alnum:]_-]|$)'

program_root="$root"
manifest=""
is_program_manifest() {
  grep -q '^\[package\]' "$1" && grep -Eq "$runtime_dep_regex" "$1"
}

if [[ -f "$root/Cargo.toml" ]] && is_program_manifest "$root/Cargo.toml"; then
  manifest="$root/Cargo.toml"
else
  candidates=()
  while IFS= read -r m; do
    is_program_manifest "$m" && candidates+=("$m")
  done < <(find "$root" -type f -name Cargo.toml -not -path '*/target/*' -not -path '*/.git/*' | sort)
  if [[ ${#candidates[@]} -eq 1 ]]; then
    manifest="${candidates[0]}"
    program_root="$(cd "$(dirname "$manifest")" && pwd -P)"
  elif [[ ${#candidates[@]} -gt 1 ]]; then
    echo "error: multiple program crates found; pass --root pointing at one" >&2
    printf 'candidate_program=%s\n' "${candidates[@]}" >&2
    exit 2
  elif [[ -f "$root/Cargo.toml" ]]; then
    manifest="$root/Cargo.toml"
  fi
fi

runtime="unknown"
if [[ -n "$manifest" ]]; then
  if grep -Eq '(^|[^[:alnum:]_-])anchor-lang([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="anchor"
  elif grep -Eq '(^|[^[:alnum:]_-])pinocchio([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="pinocchio"
  elif grep -Eq 'quasar-lang' "$manifest" || grep -R -q --include='*.rs' --exclude-dir=target --exclude-dir=.git '#\[qed(verified' "$program_root" 2>/dev/null; then
    runtime="qedgen-codegen"
  elif grep -Eq '(^|[^[:alnum:]_-])solana-program([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="native-rust"
  fi
fi

rust_source="$(find "$program_root" -type f -name '*.rs' -not -path '*/target/*' -print -quit)"
asm_source="$(find "$program_root" -type f -name '*.s' -not -path '*/target/*' -print -quit)"
if [[ -n "$asm_source" && -z "$rust_source" ]]; then
  runtime="sbpf-assembly"
fi

mode="spec-less"
[[ -n "$spec" ]] && mode="spec-aware"

qed_manifest=""
if [[ -n "$spec" && -f "$(dirname "$spec")/qed.toml" ]]; then
  qed_manifest="$(dirname "$spec")/qed.toml"
elif [[ -f "$root/qed.toml" ]]; then
  qed_manifest="$root/qed.toml"
fi

qedgen_status="missing"
qedgen_version=""
if command -v "$qedgen_bin" >/dev/null 2>&1 || [[ -x "$qedgen_bin" ]]; then
  qedgen_version="$($qedgen_bin --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+\.[0-9]+\.[0-9]+).*/\1/' | head -1)"
  if [[ -n "$qedgen_version" ]] && version_at_least "$qedgen_version" "$MIN_QEDGEN_VERSION"; then
    qedgen_status="ready"
  else
    qedgen_status="stale"
  fi
fi

skill_commit="unknown"
if [[ -f "$skill_root/SOURCE_COMMIT" ]]; then
  skill_commit="$(tr -d '[:space:]' < "$skill_root/SOURCE_COMMIT")"
elif command -v git >/dev/null 2>&1; then
  skill_commit="$(git -C "$skill_root" rev-parse HEAD 2>/dev/null || echo unknown)"
fi

echo "target_root=$root"
echo "program_root=$program_root"
echo "skill_version=$skill_version"
echo "skill_commit=$skill_commit"
echo "runtime=$runtime"
echo "mode=$mode"
echo "spec=${spec:-none}"
echo "qed_manifest=${qed_manifest:-none}"
echo "qedgen_status=$qedgen_status"
echo "qedgen_version=${qedgen_version:-unknown}"
echo "minimum_qedgen_version=$MIN_QEDGEN_VERSION"
command -v crucible >/dev/null 2>&1 && echo "crucible=ready" || echo "crucible=unavailable"
cargo +nightly miri --version >/dev/null 2>&1 && echo "miri=ready" || echo "miri=unavailable"

if [[ "$runtime" == "sbpf-assembly" && "$mode" == "spec-less" ]]; then
  echo "audit_capability=unsupported-source-audit"
elif [[ "$qedgen_status" == "ready" && ( "$runtime" != "unknown" || "$mode" == "spec-aware" ) ]]; then
  echo "audit_capability=full"
else
  echo "audit_capability=read-only"
fi

if [[ $run_compile -eq 1 ]]; then
  if [[ -z "$manifest" || ! -f "$manifest" ]]; then
    echo "compile_status=not-applicable"
  elif cargo check --quiet --manifest-path "$manifest"; then
    echo "compile_status=clean"
  else
    status=$?
    echo "compile_status=failed"
    exit "$status"
  fi
else
  echo "compile_status=not-run"
fi
