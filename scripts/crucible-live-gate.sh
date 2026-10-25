#!/usr/bin/env bash
# A requested live gate must run each named generated-artifact test, never zero tests.
set -euo pipefail
command -v crucible >/dev/null
cargo build-sbf --version
run_live() {
  local live_test=$1 gate_env=$2
  local listing
  listing=$(cargo test -p qedgen-solana-skills --test crucible_brownfield_smoke "$live_test" -- --exact --list)
  if ! printf '%s\n' "$listing" | grep -Fxq "$live_test: test"; then
    echo "Required Crucible live regression is missing: $live_test" >&2
    exit 1
  fi
  env "$gate_env=1" cargo test -p qedgen-solana-skills --test crucible_brownfield_smoke "$live_test" -- --exact --nocapture
}
run_live live_domain_boundary_protocol_misses_and_domain_finds QEDGEN_RUN_LIVE_CRUCIBLE_DOMAIN_BOUNDARY
run_live live_stored_state_buggy_fires_and_corrected_is_clean QEDGEN_RUN_LIVE_CRUCIBLE_STORED_STATE
