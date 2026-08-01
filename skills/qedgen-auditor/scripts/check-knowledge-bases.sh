#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
catalog="${QEDGEN_CATEGORY_CATALOG:-$repo_root/skills/qedgen-auditor/references/category-catalog.md}"
primer="${QEDGEN_SECURITY_PRIMER:-$repo_root/docs/security-primer.md}"
allowlist="${QEDGEN_BASIS_ALLOWLIST:-$repo_root/skills/qedgen-auditor/references/basis-legacy-allowlist.txt}"
corpus_registry="${QEDGEN_BASIS_CORPUS_REGISTRY:-$repo_root/skills/qedgen-auditor/references/basis-corpus-registry.txt}"

fail=0
uncovered=()
prose_only=()

if [[ ! -f "$catalog" || ! -f "$primer" || ! -f "$allowlist" ||
      ! -f "$corpus_registry" ]]; then
  echo "knowledge-base input missing: catalog=$catalog primer=$primer allowlist=$allowlist corpus_registry=$corpus_registry" >&2
  exit 1
fi

validate_basis() {
  local owner="$1"
  local basis="$2"

  case "$basis" in
    source:*)
      if [[ ! "$basis" =~ ^source:[^@[:space:]]+@[^:[:space:]]+:.+ ]]; then
        echo "invalid source basis for $owner: $basis" >&2
        fail=1
      fi
      ;;
    url:https://*)
      if [[ "$basis" =~ [[:space:]] ]]; then
        echo "invalid URL basis for $owner: $basis" >&2
        fail=1
      fi
      ;;
    fixture:*)
      local relative="${basis#fixture:}"
      if [[ "$relative" != crates/qedgen/tests/fixtures/* ]] ||
         [[ "$relative" == *"/../"* || "$relative" == ../* ]] ||
         [[ ! -e "$repo_root/$relative" ]]; then
        echo "fixture basis does not exist for $owner: $relative" >&2
        fail=1
      fi
      ;;
    corpus:*)
      if [[ ! "$basis" =~ ^corpus:[A-Za-z0-9._/-]+$ ]]; then
        echo "invalid corpus basis for $owner: $basis" >&2
        fail=1
      elif ! grep -Ev '^[[:space:]]*(#|$)' "$corpus_registry" |
        grep -Fxq "${basis#corpus:}"; then
        echo "corpus basis is not registered for $owner: ${basis#corpus:}" >&2
        fail=1
      fi
      ;;
    prose:*)
      local summary="${basis#prose:}"
      if [[ -z "$summary" ]] || ! grep -Fxq "$owner" "$allowlist"; then
        echo "prose basis is not allowlisted: $owner" >&2
        fail=1
      else
        prose_only+=("$owner")
      fi
      ;;
    *)
      echo "unsupported Basis for $owner: $basis" >&2
      fail=1
      ;;
  esac
}

while IFS=$'\t' read -r name count basis; do
  if [[ "$count" -eq 0 ]]; then
    echo "catalog entry missing Basis: $name" >&2
    fail=1
    continue
  fi
  if [[ "$count" -ne 1 ]]; then
    echo "catalog entry must have exactly one Basis: $name (found $count)" >&2
    fail=1
    continue
  fi
  validate_basis "$name" "$basis"
  if [[ "$basis" != fixture:* && "$basis" != corpus:* ]]; then
    uncovered+=("$name")
  fi
done < <(
  awk '
    function emit() {
      if (name != "") print name "\t" count "\t" basis
    }
    /^### `/ {
      emit()
      name = $0
      sub(/^### `/, "", name)
      sub(/`.*/, "", name)
      count = 0
      basis = ""
      next
    }
    /^Basis:/ && name != "" {
      count++
      if (count == 1) {
        basis = $0
        sub(/^Basis:[[:space:]]*/, "", basis)
      }
    }
    END { emit() }
  ' "$catalog"
)

while IFS=$'\t' read -r section count basis; do
  if [[ "$count" -eq 0 ]]; then
    echo "primer Grep for block missing Basis: $section" >&2
    fail=1
    continue
  fi
  if [[ "$count" -ne 1 ]]; then
    echo "primer Grep for block must have exactly one Basis: $section (found $count)" >&2
    fail=1
    continue
  fi
  validate_basis "primer:$section" "$basis"
done < <(
  # `**Grep for:**` opens the block and `**Basis:**` must appear inside it, so
  # in the primer the Basis line has to sit AFTER its `**Grep for:**` header,
  # not before. Moving it above the header would read better but would leave
  # the block with no Basis and fail this gate.
  awk '
    function emit() {
      if (active) print section "\t" count "\t" basis
    }
    /^### / {
      if (active) emit()
      section = $0
      sub(/^### /, "", section)
      active = 0
      count = 0
      basis = ""
      next
    }
    /^\*\*Grep for:\*\*/ {
      if (active) emit()
      active = 1
      count = 0
      basis = ""
      next
    }
    /^\*\*Basis:\*\*/ && active {
      count++
      if (count == 1) {
        basis = $0
        sub(/^\*\*Basis:\*\*[[:space:]]*/, "", basis)
      }
    }
    END { if (active) emit() }
  ' "$primer"
)

if ((${#prose_only[@]} > 0)); then
  sorted_prose="$(printf '%s\n' "${prose_only[@]}" | sort -u)"
  prose_count="$(wc -l <<<"$sorted_prose" | tr -d ' ')"
  prose_summary="$(paste -sd, - <<<"$sorted_prose" | sed 's/,/, /g')"
  echo "warning: allowlisted legacy prose-only bases ($prose_count): $prose_summary" >&2
fi

if ((${#uncovered[@]} > 0)); then
  sorted_uncovered="$(printf '%s\n' "${uncovered[@]}" | sort -u)"
  uncovered_count="$(wc -l <<<"$sorted_uncovered" | tr -d ' ')"
  uncovered_summary="$(paste -sd, - <<<"$sorted_uncovered" | sed 's/,/, /g')"
  echo "warning: catalog categories without fixture/corpus basis ($uncovered_count): $uncovered_summary" >&2
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "auditor knowledge bases valid"
