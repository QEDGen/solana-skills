//! Shared helpers, constants, and param factories for proof files.

pub use percolator::*;
pub use percolator::i128::{I128, U128};
pub use percolator::wide_math::{
    U256, I256,
    floor_div_signed_conservative,
    saturating_mul_u256_u64,
    fee_debt_u128_checked,
    mul_div_floor_u256,
    mul_div_floor_u256_with_rem,
    mul_div_ceil_u256,
    wide_signed_mul_div_floor,
    ceil_div_positive_checked,
    mul_div_floor_u128,
    mul_div_ceil_u128,
    wide_mul_div_floor_u128,
    wide_signed_mul_div_floor_from_k_pair,
    saturating_mul_u128_u64,
};

// ============================================================================
// Small-model constants
// ============================================================================

/// Small-model scale factors (minimal bit-widths for CBMC tractability).
/// All arithmetic stays within i32/u16 to avoid 64-bit SAT blowup.
pub const S_POS_SCALE: u16 = 4;
pub const S_ADL_ONE: u16 = 256;

// ============================================================================
// Engine constants
// ============================================================================

pub const DEFAULT_ORACLE: u64 = 1_000;
pub const DEFAULT_SLOT: u64 = 100;

// ============================================================================
// Small-model helpers
// ============================================================================

/// Small-model: eager PnL for one mark event (long).
pub fn eager_mark_pnl_long(q_base: i32, delta_p: i32) -> i32 {
    q_base * delta_p
}

/// Small-model: eager PnL for one mark event (short).
pub fn eager_mark_pnl_short(q_base: i32, delta_p: i32) -> i32 {
    -(q_base * delta_p)
}

/// Small-model: lazy PnL from K difference.
/// pnl_delta = floor(|basis_q| * (K_cur - k_snap) / (a_basis * POS_SCALE))
pub fn lazy_pnl(basis_q_abs: u16, k_diff: i32, a_basis: u16) -> i32 {
    let den = (a_basis as i32) * (S_POS_SCALE as i32);
    if den == 0 { return 0; }
    let num = (basis_q_abs as i32) * k_diff;
    if num >= 0 {
        num / den
    } else {
        let abs_num = -num;
        -((abs_num + den - 1) / den)
    }
}

/// Small-model: lazy effective quantity.
pub fn lazy_eff_q(basis_q_abs: u16, a_cur: u16, a_basis: u16) -> u16 {
    if a_basis == 0 { return 0; }
    let product = (basis_q_abs as i32) * (a_cur as i32);
    (product / (a_basis as i32)) as u16
}

/// Small-model: K update for mark event (long).
pub fn k_after_mark_long(k_before: i32, a_long: u16, delta_p: i32) -> i32 {
    k_before + (a_long as i32) * delta_p
}

/// Small-model: K update for mark event (short).
pub fn k_after_mark_short(k_before: i32, a_short: u16, delta_p: i32) -> i32 {
    k_before - (a_short as i32) * delta_p
}

/// Small-model: K update for funding event (long).
pub fn k_after_fund_long(k_before: i32, a_long: u16, delta_f: i32) -> i32 {
    k_before - (a_long as i32) * delta_f
}

/// Small-model: K update for funding event (short).
pub fn k_after_fund_short(k_before: i32, a_short: u16, delta_f: i32) -> i32 {
    k_before + (a_short as i32) * delta_f
}

/// Small-model: A update for ADL quantity shrink.
pub fn a_after_adl(a_old: u16, oi_post: u16, oi: u16) -> u16 {
    if oi == 0 { return a_old; }
    let product = (a_old as i32) * (oi_post as i32);
    (product / (oi as i32)) as u16
}

// ============================================================================
// Engine param helpers
// ============================================================================

pub fn zero_fee_params() -> RiskParams {
    RiskParams {
        warmup_period_slots: 100,
        maintenance_margin_bps: 500,
        initial_margin_bps: 1000,
        trading_fee_bps: 0,
        max_accounts: MAX_ACCOUNTS as u64,
        new_account_fee: U128::ZERO,
        maintenance_fee_per_slot: U128::ZERO,
        max_crank_staleness_slots: u64::MAX,
        liquidation_fee_bps: 0,
        liquidation_fee_cap: U128::ZERO,
        liquidation_buffer_bps: 50,
        min_liquidation_abs: U128::ZERO,
        min_initial_deposit: U128::new(2),
        min_nonzero_mm_req: 1,
        min_nonzero_im_req: 2,
        insurance_floor: U128::ZERO,
    }
}

pub fn default_params() -> RiskParams {
    RiskParams {
        warmup_period_slots: 100,
        maintenance_margin_bps: 500,
        initial_margin_bps: 1000,
        trading_fee_bps: 10,
        max_accounts: MAX_ACCOUNTS as u64,
        new_account_fee: U128::new(1000),
        maintenance_fee_per_slot: U128::new(1),
        max_crank_staleness_slots: 1000,
        liquidation_fee_bps: 100,
        liquidation_fee_cap: U128::new(1_000_000),
        liquidation_buffer_bps: 50,
        min_liquidation_abs: U128::new(0),
        min_initial_deposit: U128::new(1000),
        min_nonzero_mm_req: 1,
        min_nonzero_im_req: 2,
        insurance_floor: U128::ZERO,
    }
}
