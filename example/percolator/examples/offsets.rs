use std::mem::{size_of, offset_of};
fn main() {
    use percolator::RiskEngine;
    println!("sizeof RiskEngine = {}", size_of::<RiskEngine>());
    println!("c_tot: {}", offset_of!(RiskEngine, c_tot));
    println!("pnl_pos_tot: {}", offset_of!(RiskEngine, pnl_pos_tot));
    println!("pnl_matured_pos_tot: {}", offset_of!(RiskEngine, pnl_matured_pos_tot));
    println!("adl_mult_long: {}", offset_of!(RiskEngine, adl_mult_long));
    println!("adl_mult_short: {}", offset_of!(RiskEngine, adl_mult_short));
    println!("adl_epoch_long: {}", offset_of!(RiskEngine, adl_epoch_long));
    println!("adl_epoch_short: {}", offset_of!(RiskEngine, adl_epoch_short));
    println!("side_mode_long: {}", offset_of!(RiskEngine, side_mode_long));
    println!("insurance_floor: {}", offset_of!(RiskEngine, insurance_floor));
    println!("num_used_accounts: {}", offset_of!(RiskEngine, num_used_accounts));
    println!("accounts: {}", offset_of!(RiskEngine, accounts));
    use percolator::Account;
    println!("sizeof Account = {}", size_of::<Account>());
    println!("capital: {}", offset_of!(Account, capital));
    println!("pnl: {}", offset_of!(Account, pnl));
    println!("position_basis_q: {}", offset_of!(Account, position_basis_q));
    println!("adl_a_basis: {}", offset_of!(Account, adl_a_basis));
    println!("adl_epoch_snap: {}", offset_of!(Account, adl_epoch_snap));
}
