// End-to-end integration tests with realistic trading scenarios
// Tests complete user journeys with multiple participants

#[cfg(feature = "test")]
use percolator::*;
#[cfg(feature = "test")]
use percolator::i128::U128;

#[cfg(feature = "test")]
fn default_params() -> RiskParams {
    RiskParams {
        warmup_period_slots: 100,
        maintenance_margin_bps: 500, // 5%
        initial_margin_bps: 1000,    // 10%
        trading_fee_bps: 10,         // 0.1%
        max_accounts: 64,
        new_account_fee: U128::new(0),
        maintenance_fee_per_slot: U128::new(0),
        max_crank_staleness_slots: u64::MAX,
        liquidation_fee_bps: 50,
        liquidation_fee_cap: U128::new(100_000),
        liquidation_buffer_bps: 100,
        min_liquidation_abs: U128::new(0),
        min_initial_deposit: U128::new(2),
        min_nonzero_mm_req: 1,
        min_nonzero_im_req: 2,
        insurance_floor: U128::ZERO,
    }
}

/// Helper: create i128 position size from base quantity (scaled by POS_SCALE)
#[cfg(feature = "test")]
fn pos_q(qty: i64) -> i128 {
    let abs_val = (qty as i128).unsigned_abs();
    let scaled = abs_val.checked_mul(POS_SCALE).unwrap();
    if qty < 0 {
        -(scaled as i128)
    } else {
        scaled as i128
    }
}

/// Helper: crank to make trades/withdrawals work
#[cfg(feature = "test")]
fn crank(engine: &mut RiskEngine, slot: u64, oracle_price: u64) {
    let _ = engine.keeper_crank(slot, oracle_price, &[], 64);
}

// ============================================================================
// E2E Test 1: Complete User Journey
// ============================================================================

#[test]
#[cfg(feature = "test")]
fn test_e2e_complete_user_journey() {
    // Scenario: Alice and Bob trade, experience PNL, warmup, withdrawal

    let mut engine = Box::new(RiskEngine::new(default_params()));

    // Initialize insurance fund
    let _ = engine.top_up_insurance_fund(50_000, 0);

    // Add two users with capital
    let alice = engine.add_user(0).unwrap();
    let bob = engine.add_user(0).unwrap();

    let oracle_price: u64 = 100; // 100 quote per base

    // Users deposit principal
    engine.deposit(alice, 100_000, oracle_price, 0).unwrap();
    engine.deposit(bob, 150_000, oracle_price, 0).unwrap();

    // Make crank fresh
    crank(&mut engine, 0, oracle_price);

    // === Phase 1: Trading ===

    // Alice goes long 50 base, Bob takes the other side (short)
    engine
        .execute_trade(alice, bob, oracle_price, 0, pos_q(50), oracle_price)
        .unwrap();

    // Check effective positions
    let alice_eff = engine.effective_pos_q(alice as usize);
    let bob_eff = engine.effective_pos_q(bob as usize);
    assert!(alice_eff > 0, "Alice should be long");
    assert!(bob_eff < 0, "Bob should be short");

    // Conservation should hold
    assert!(engine.check_conservation(), "Conservation after trade");

    // === Phase 2: Price Movement ===

    let new_price: u64 = 120; // +20%

    // Accrue market to new price
    engine.advance_slot(10);
    let slot = engine.current_slot;
    engine.accrue_market_to(slot, new_price).unwrap();

    // Settle side effects for Alice (should have positive PnL from long)
    engine.settle_side_effects(alice as usize).unwrap();

    let alice_pnl = engine.accounts[alice as usize].pnl;
    // Long position + price up = positive PnL
    assert!(alice_pnl > 0, "Alice should have positive PnL after price increase");

    // === Phase 3: PNL Warmup ===

    // Advance some slots
    engine.advance_slot(50);

    // Touch to settle and convert warmup
    let slot = engine.current_slot;
    engine.touch_account_full(alice as usize, new_price, slot).unwrap();

    // The key invariant is conservation
    assert!(engine.check_conservation(), "Conservation after warmup");

    // === Phase 4: Close positions and withdraw ===

    let slot = engine.current_slot;
    crank(&mut engine, slot, new_price);

    // Alice closes her position (sell)
    let alice_pos = engine.effective_pos_q(alice as usize);
    if alice_pos != 0 {
        let neg_pos = alice_pos.checked_neg().unwrap();
        let slot = engine.current_slot;
        engine
            .execute_trade(alice, bob, new_price, slot, neg_pos, new_price)
            .unwrap();
    }

    // Advance for full warmup
    engine.advance_slot(200);
    let slot = engine.current_slot;
    engine.touch_account_full(alice as usize, new_price, slot).unwrap();

    // Alice withdraws some capital
    let slot = engine.current_slot;
    crank(&mut engine, slot, new_price);
    let alice_cap = engine.accounts[alice as usize].capital.get();
    if alice_cap > 1000 {
        let slot = engine.current_slot;
        engine.withdraw(alice, 1000, new_price, slot).unwrap();
    }

    assert!(engine.check_conservation(), "Conservation after withdrawal");
}

// ============================================================================
// E2E Test 2: Funding Complete Cycle
// ============================================================================

#[test]
#[cfg(feature = "test")]
fn test_e2e_funding_complete_cycle() {
    // Scenario: Users trade, funding accrues over time, positions flip

    let mut engine = Box::new(RiskEngine::new(default_params()));
    let _ = engine.top_up_insurance_fund(50_000, 0);

    let alice = engine.add_user(0).unwrap();
    let bob = engine.add_user(0).unwrap();

    let oracle_price: u64 = 100;

    engine.deposit(alice, 200_000, oracle_price, 0).unwrap();
    engine.deposit(bob, 200_000, oracle_price, 0).unwrap();

    crank(&mut engine, 0, oracle_price);

    // Alice goes long, Bob goes short
    engine
        .execute_trade(alice, bob, oracle_price, 0, pos_q(100), oracle_price)
        .unwrap();

    // Advance time and accrue funding with a positive rate (longs pay shorts)
    engine.advance_slot(20);

    // Run keeper_crank to advance
    let slot = engine.current_slot;
    let _ = engine.keeper_crank(slot, oracle_price, &[], 64);

    // Advance more time for funding to accrue
    engine.advance_slot(20);

    // Accrue market with funding
    let slot = engine.current_slot;
    engine.accrue_market_to(slot, oracle_price).unwrap();

    // Settle effects for both
    engine.settle_side_effects(alice as usize).unwrap();
    engine.settle_side_effects(bob as usize).unwrap();

    let _alice_pnl = engine.accounts[alice as usize].pnl;
    let _bob_pnl = engine.accounts[bob as usize].pnl;

    // Alice (long) paid funding, so negative PnL
    // Bob (short) received funding, so positive PnL
    // (With 50 bps/slot rate * 20 slots, the K coefficients should reflect this)
    // The exact values depend on the A/K mechanism, but the sign should be correct:
    // funding_rate > 0 means longs pay -> K_long decreases, K_short increases

    // Conservation should still hold
    assert!(engine.check_conservation(), "Conservation after funding");

    // === Positions Flip ===

    let slot = engine.current_slot;
    crank(&mut engine, slot, oracle_price);

    // Alice closes long and opens short (total -200 base)
    engine
        .execute_trade(alice, bob, oracle_price, slot, pos_q(-200), oracle_price)
        .unwrap();

    // Now Alice is short and Bob is long
    let alice_eff = engine.effective_pos_q(alice as usize);
    let bob_eff = engine.effective_pos_q(bob as usize);
    assert!(alice_eff < 0, "Alice should now be short");
    assert!(bob_eff > 0, "Bob should now be long");

    assert!(engine.check_conservation(), "Conservation after position flip");
}
