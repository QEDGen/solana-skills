#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
rust_source="${QEDGEN_CATEGORY_RUST:-$repo_root/crates/qedgen/src/probe/mod.rs}"
catalog="${QEDGEN_CATEGORY_CATALOG:-$repo_root/skills/qedgen-auditor/references/category-catalog.md}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Category::tag() is the Rust serialization boundary. Capture the first tag
# literal following each match arm, including rustfmt's multi-line arms.
awk '
  /pub fn tag\(&self\)/ { in_tag = 1 }
  /pub enum Severity/ { in_tag = 0 }
  in_tag && /Category::/ { pending = 1 }
  in_tag && pending && match($0, /"[a-z][a-z0-9_]*"/) {
    print substr($0, RSTART + 1, RLENGTH - 2)
    pending = 0
  }
' "$rust_source" | sort -u > "$tmp/rust"

sed -n 's/^### `\([^`]*\)`.*/\1/p' "$catalog" | sort -u > "$tmp/catalog"

# These Rust identities intentionally stay probe-internal: they are produced
# mechanically and do not represent a model work-list lens.
cat > "$tmp/probe-only" <<'EOF'
crucible_fuzz_crash
execution_divergence
external_authority_not_revoked_on_close
graceful_error_as_dos
idl_source_drift
init_without_pda
paired_validator_input_domain_mismatch
silent_success_arithmetic
unbounded_amount_param
unchecked_arith_with_fund_flow
unwired_error_variant
EOF

# These catalog entries are intentionally model-only: the worker investigates
# them from source/domain context and no deterministic probe predicate owns
# their identity.
cat > "$tmp/model-only" <<'EOF'
account_not_reloaded_after_cpi
account_type_confusion
authority_transfer_missing_nominate_accept
cleanup_incentive_mismatch
close_account_redirection
compressed_nft_ownership_unverified
custody_terms_retroactive_mutation
discriminator_collision
duplicate_mutable_accounts_aliasing
execution_order_state_before_check
flag_branch_no_op
flash_loan_amplified_governance
frontrunnable_no_slippage
init_without_is_initialized
lamport_balance_not_program_controlled
lamport_write_demotion
liquidation_rounding_dust_accumulation
missing_owner_check
missing_rent_exemption_check_on_init
oracle_staleness
payout_exceeds_recorded_principal
pda_lifecycle_reuse_after_close
pda_seed_collision
permissionless_instruction_no_rate_limit
privileged_action_no_delay_window
realloc_zero_init_data_leak
rounding_direction_round_trip
sentinel_null_key_array_short_circuit
token_2022_extension_arithmetic_skew
token_account_role_anchoring
transfer_hook_untrusted_callback
twap_gameable_single_block
unvalidated_remaining_accounts
EOF

sort -u "$tmp/probe-only" -o "$tmp/probe-only"
sort -u "$tmp/model-only" -o "$tmp/model-only"

comm -23 "$tmp/rust" "$tmp/catalog" > "$tmp/rust-orphans"
comm -13 "$tmp/rust" "$tmp/catalog" > "$tmp/catalog-orphans"
comm -23 "$tmp/rust-orphans" "$tmp/probe-only" > "$tmp/unallowed-rust"
comm -23 "$tmp/catalog-orphans" "$tmp/model-only" > "$tmp/unallowed-catalog"

if [[ -s "$tmp/unallowed-rust" || -s "$tmp/unallowed-catalog" ]]; then
  echo "category identity drift between Category::tag() and category-catalog.md" >&2
  if [[ -s "$tmp/unallowed-rust" ]]; then
    echo "Rust categories missing from catalog/allowlist:" >&2
    sed 's/^/  /' "$tmp/unallowed-rust" >&2
  fi
  if [[ -s "$tmp/unallowed-catalog" ]]; then
    echo "catalog categories missing from Rust/allowlist:" >&2
    sed 's/^/  /' "$tmp/unallowed-catalog" >&2
  fi
  exit 1
fi

echo "category identities reconciled"
