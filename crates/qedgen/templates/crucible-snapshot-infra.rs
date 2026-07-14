// ── Protocol invariant suite (crash-first, spec-less) ─────────────────
// Guards read only post-state (get_account: lamports / owner / data). A
// fault INSIDE the program (overflow, unwrap, require!) surfaces as a TX
// error, not a host panic, so it is NOT caught here — that stays the
// Mollusk / spec lane. Favor-coverage: guards fire on any candidate shape
// and lean on downstream triage.
// ──────────────────────────────────────────────────────────────────────
#[derive(Clone)]
struct AccountSnapshot {
    pk: Pubkey,
    exists: bool,
    lamports: u64,
    owner: Pubkey,
    disc: [u8; 8],
    data_len: usize,
    // SPL / Token-2022 account fields (Some when owner is a token program and
    // data is at least the 165-byte base account layout). mint @ 0, amount @ 64.
    token_mint: Option<Pubkey>,
    token_amount: u64,
}

/// SPL Token + Token-2022 program ids (base58). Compared against `owner` to
/// classify token accounts spec-lessly. String compare avoids a FromStr
/// dependency and is cheap enough for a fuzz harness.
fn is_token_program(owner: &Pubkey) -> bool {
    let s = owner.to_string();
    s == "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        || s == "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
}

fn account_state(ctx: &TestContext, pk: &Pubkey) -> AccountSnapshot {
    match ctx.svm.get_account(pk) {
        Some(a) => {
            let mut disc = [0u8; 8];
            let n = a.data.len().min(8);
            disc[..n].copy_from_slice(&a.data[..n]);
            // Token account = owned by a token program with the 165-byte base
            // layout (mints are 82 bytes, so they're excluded).
            let (token_mint, token_amount) = if is_token_program(&a.owner) && a.data.len() >= 165 {
                let mut mint = [0u8; 32];
                mint.copy_from_slice(&a.data[0..32]);
                let mut amt = [0u8; 8];
                amt.copy_from_slice(&a.data[64..72]);
                (Some(Pubkey::new_from_array(mint)), u64::from_le_bytes(amt))
            } else {
                (None, 0)
            };
            AccountSnapshot {
                pk: *pk,
                exists: true,
                lamports: a.lamports,
                owner: a.owner,
                disc,
                data_len: a.data.len(),
                token_mint,
                token_amount,
            }
        }
        None => AccountSnapshot {
            pk: *pk,
            exists: false,
            lamports: 0,
            owner: Pubkey::default(),
            disc: [0u8; 8],
            data_len: 0,
            token_mint: None,
            token_amount: 0,
        },
    }
}

fn snapshot_account_state(ctx: &TestContext, tracked: &[Pubkey]) -> Vec<AccountSnapshot> {
    tracked.iter().map(|pk| account_state(ctx, pk)).collect()
}

fn rent_exempt_minimum(data_len: usize) -> u64 {
    anchor_lang::solana_program::rent::Rent::default().minimum_balance(data_len)
}
