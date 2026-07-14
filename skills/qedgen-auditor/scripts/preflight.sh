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

manifest="$root/Cargo.toml"
runtime="unknown"
if [[ -f "$manifest" ]]; then
  if grep -Eq '(^|[^[:alnum:]_-])anchor-lang([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="anchor"
  elif grep -Eq '(^|[^[:alnum:]_-])pinocchio([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="pinocchio"
  elif grep -Eq 'quasar-lang' "$manifest" || grep -R -q --include='*.rs' --exclude-dir=target --exclude-dir=.git '#\[qed(verified' "$root" 2>/dev/null; then
    runtime="qedgen-codegen"
  elif grep -Eq '(^|[^[:alnum:]_-])solana-program([^[:alnum:]_-]|$)' "$manifest"; then
    runtime="native-rust"
  fi
fi

rust_source="$(find "$root" -type f -name '*.rs' -not -path '*/target/*' -print -quit)"
asm_source="$(find "$root" -type f -name '*.s' -not -path '*/target/*' -print -quit)"
if [[ -n "$asm_source" && -z "$rust_source" ]]; then
  runtime="sbpf-assembly"
fi

mode="spec-less"
[[ -n "$spec" ]] && mode="spec-aware"

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

echo "target_root=$root"
echo "skill_version=$skill_version"
echo "runtime=$runtime"
echo "mode=$mode"
echo "spec=${spec:-none}"
echo "qedgen_status=$qedgen_status"
echo "qedgen_version=${qedgen_version:-unknown}"
echo "minimum_qedgen_version=$MIN_QEDGEN_VERSION"
command -v crucible >/dev/null 2>&1 && echo "crucible=ready" || echo "crucible=unavailable"
cargo +nightly miri --version >/dev/null 2>&1 && echo "miri=ready" || echo "miri=unavailable"

if [[ "$runtime" == "sbpf-assembly" && "$mode" == "spec-less" ]]; then
  echo "audit_capability=unsupported-source-audit"
elif [[ "$qedgen_status" == "ready" ]]; then
  echo "audit_capability=full"
else
  echo "audit_capability=read-only"
fi

if [[ $run_compile -eq 1 ]]; then
  if [[ ! -f "$manifest" ]]; then
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
