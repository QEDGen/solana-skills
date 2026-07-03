use super::*;
use crate::check::{ImportedNamespace, ParsedAccountType, ParsedVariant};

fn empty_spec() -> ParsedSpec {
    ParsedSpec::default()
}

fn spec_with_constants(pairs: &[(&str, &str)]) -> ParsedSpec {
    ParsedSpec {
        constants: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..ParsedSpec::default()
    }
}

#[test]
fn map_type_covers_all_primitives() {
    let spec = empty_spec();

    // Integer primitives
    assert_eq!(map_type("U8", &spec).unwrap(), "u8");
    assert_eq!(map_type("U16", &spec).unwrap(), "u16");
    assert_eq!(map_type("U32", &spec).unwrap(), "u32");
    assert_eq!(map_type("U64", &spec).unwrap(), "u64");
    assert_eq!(map_type("U128", &spec).unwrap(), "u128");
    assert_eq!(map_type("I8", &spec).unwrap(), "i8");
    assert_eq!(map_type("I16", &spec).unwrap(), "i16");
    assert_eq!(map_type("I32", &spec).unwrap(), "i32");
    assert_eq!(map_type("I64", &spec).unwrap(), "i64");
    assert_eq!(map_type("I128", &spec).unwrap(), "i128");

    // Non-integer primitives
    assert_eq!(map_type("Bool", &spec).unwrap(), "bool");
    // v2.21 Slice 3: Standalone Pubkey lowers to [u8; 32] (was
    // "Address" pre-v2.21; the alias is retired).
    assert_eq!(map_type("Pubkey", &spec).unwrap(), "[u8; 32]");
}

#[test]
fn map_type_anchor_uses_native_pubkey() {
    let spec = empty_spec();

    assert_eq!(map_type_anchor("Pubkey", &spec).unwrap(), "Pubkey");
    assert_eq!(
        map_type_anchor("Map[2] Pubkey", &spec).unwrap(),
        "[Pubkey; 2]"
    );
}

#[test]
fn framework_surface_centralizes_target_snippets() {
    let anchor = FrameworkSurface::for_target(Target::Anchor);
    assert_eq!(
        anchor.token_account_type(true),
        "Account<'info, TokenAccount>"
    );
    assert_eq!(
        anchor.program_type("token_program", None, false),
        "Program<'info, Token>"
    );
    assert_eq!(
        anchor.error_expr("EscrowError", "Unauthorized"),
        "EscrowError::Unauthorized.into()"
    );
    assert_eq!(
        anchor.authority_check_expr("escrow_ta", "escrow"),
        "ctx.escrow_ta.owner != ctx.escrow.key()"
    );

    let quasar = FrameworkSurface::for_target(Target::Quasar);
    assert_eq!(quasar.token_account_type(true), "&'info mut Account<Token>");
    assert_eq!(
        quasar.program_type("token_program", None, false),
        "&'info Program<Token>"
    );
    assert_eq!(
        quasar.program_type("system_program", None, false),
        "&'info Program<System>"
    );
    assert_eq!(
        quasar.error_expr("EscrowError", "Unauthorized"),
        "ProgramError::from(EscrowError::Unauthorized)"
    );
    assert_eq!(
        quasar.authority_check_expr("escrow_ta", "escrow"),
        "(*ctx.escrow_ta.owner()) != (*ctx.escrow.to_account_view().address())"
    );
}

#[test]
fn map_type_errors_on_unknown_type() {
    // v2.6.1 bug: DSL types not in the four-item allowlist (U8/U64/U128/I128)
    // fell through as-is, leaking `U16` verbatim into Rust. v2.6.2: unknown
    // types must surface as errors at codegen time.
    let spec = empty_spec();
    let err = map_type("Blorb", &spec).unwrap_err().to_string();
    assert!(
        err.contains("Blorb"),
        "error should name the bad type: {err}"
    );
    assert!(
        err.contains("unsupported DSL type"),
        "error should call it out as unsupported: {err}"
    );
}

#[test]
fn map_type_renders_map_with_literal_bound() {
    let spec = empty_spec();
    assert_eq!(map_type("Map[4] U64", &spec).unwrap(), "[u64; 4]");
    assert_eq!(map_type("Map[16] U8", &spec).unwrap(), "[u8; 16]");
    // v2.21 Slice 3: nested Pubkey lowers through `[u8; 32]` too.
    assert_eq!(map_type("Map[32] Pubkey", &spec).unwrap(), "[[u8; 32]; 32]");
}

#[test]
fn map_type_resolves_map_bound_via_constants() {
    // Mirrors the percolator eval case: `Map[MAX_ACCOUNTS] U64` where
    // MAX_ACCOUNTS is declared as a spec constant.
    let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256"), ("UNRELATED", "99")]);
    assert_eq!(
        map_type("Map[MAX_ACCOUNTS] U64", &spec).unwrap(),
        "[u64; 256]"
    );
}

#[test]
fn map_type_errors_when_map_bound_is_unknown_symbol() {
    // Bound is neither a literal nor a declared constant → clear error
    // naming the unresolved symbol.
    let spec = empty_spec();
    let err = map_type("Map[MISSING] U64", &spec).unwrap_err().to_string();
    assert!(
        err.contains("MISSING"),
        "error should name the bound: {err}"
    );
    assert!(
        err.contains("not a numeric literal") || err.contains("not declared"),
        "error should explain why the bound didn't resolve: {err}"
    );
}

#[test]
fn map_type_resolves_fin_to_usize() {
    // `Fin[N]` → `usize`. Used for index types like `Fin[MAX_ACCOUNTS]`.
    let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256")]);
    assert_eq!(map_type("Fin[MAX_ACCOUNTS]", &spec).unwrap(), "usize");
    assert_eq!(map_type("Fin[4]", &spec).unwrap(), "usize");
}

#[test]
fn map_type_resolves_type_aliases_transitively() {
    // The percolator pattern: `type AccountIdx = Fin[MAX_ACCOUNTS]`.
    // `map_type("AccountIdx")` must resolve through the alias to `usize`.
    use crate::check::ParsedRecordType;
    let mut spec = ParsedSpec {
        type_aliases: vec![
            ("AccountIdx".to_string(), "Fin[MAX_ACCOUNTS]".to_string()),
            ("MyAlias".to_string(), "U64".to_string()),
        ],
        ..ParsedSpec::default()
    };
    assert_eq!(map_type("AccountIdx", &spec).unwrap(), "usize");
    assert_eq!(map_type("MyAlias", &spec).unwrap(), "u64");

    // Record name stays as-is for struct emission downstream.
    spec.records.push(ParsedRecordType {
        name: "UserAccount".to_string(),
        fields: vec![
            ("active".to_string(), "U8".to_string()),
            ("capital".to_string(), "U128".to_string()),
        ],
    });
    assert_eq!(map_type("UserAccount", &spec).unwrap(), "UserAccount");
    // `Map[N] UserAccount` → `[UserAccount; N]`.
    spec.constants = vec![("MAX_ACCOUNTS".to_string(), "4".to_string())];
    assert_eq!(
        map_type("Map[MAX_ACCOUNTS] UserAccount", &spec).unwrap(),
        "[UserAccount; 4]"
    );
}

#[test]
fn sanitize_ident_replaces_subscripts_and_dots() {
    // The eval's actual output:
    //   fn verify_init_user_effect_accounts[i].active()
    // must become a legal Rust identifier.
    assert_eq!(sanitize_ident("accounts[i].active"), "accounts_i_active");
    assert_eq!(sanitize_ident("s.foo.bar"), "s_foo_bar");
    assert_eq!(sanitize_ident("plain_field"), "plain_field");
}

#[test]
fn sanitize_ident_collapses_consecutive_and_trailing_underscores() {
    // Repeated non-ident chars should not pile up as `___`.
    assert_eq!(sanitize_ident("foo[ ].bar"), "foo_bar");
    // Leading non-ident chars produce a leading `_` that stays (doesn't
    // collapse to empty) — this keeps the resulting string non-empty.
    assert_eq!(sanitize_ident("[i]"), "_i");
    // Trailing non-ident chars drop cleanly.
    assert_eq!(sanitize_ident("foo."), "foo");
}

/// Slice 6 step 3 — Pinocchio state.rs is zeropod zero-copy: a
/// sum-type State lowers to a `#[repr(u8)]` discriminant tag enum +
/// a flat `#[derive(ZeroPod)]` superset struct (tag byte + every
/// variant field flattened, deduped). No `#[account]` / Anchor shape.
#[test]
fn pinocchio_state_sum_type_lowers_to_tag_plus_superset() {
    let src = r#"spec Escrow

type State
  | Uninitialized
  | Open of {
      initializer : Pubkey,
      amount      : U64,
    }
  | Closed

type Error
  | InvalidAmount
  | WrongState

handler initialize (amount : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer : signer, writable
  }
  requires amount > 0 else InvalidAmount
  effect {
    Open.initializer := initializer.pubkey
  }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let fp = crate::fingerprint::compute_fingerprint(&spec);

    // Pinocchio state emission is a shared helper (codegen_mir calls it
    // directly; the legacy generate_state pipeline no longer routes
    // Pinocchio — MIR is the path).
    let mut state = String::new();
    emit_pinocchio_state(&spec, &fp, &mut state).unwrap();

    // zeropod imports, not the pinocchio AccountInfo prelude.
    assert!(
        state.contains("use zeropod::ZeroPod;"),
        "must import zeropod; got:\n{state}"
    );
    // Discriminant tag enum from the variant names.
    assert!(
        state.contains("#[repr(u8)]")
            && state.contains("pub enum EscrowAccountTag {")
            && state.contains("Uninitialized = 0,")
            && state.contains("Open = 1,")
            && state.contains("Closed = 2,"),
        "must emit a #[repr(u8)] tag enum from variants; got:\n{state}"
    );
    // Flat ZeroPod superset struct: tag byte + flattened variant fields
    // (Pubkey -> [u8; 32], U64 -> u64; the derive makes the Pod companion).
    assert!(
        state.contains("#[derive(ZeroPod)]\npub struct EscrowAccount {")
            && state.contains("pub tag: u8,")
            && state.contains("pub initializer: [u8; 32],")
            && state.contains("pub amount: u64,"),
        "must flatten variant payloads into one ZeroPod struct; got:\n{state}"
    );
    // No Anchor/Quasar shape leakage.
    assert!(
        !state.contains("#[account]") && !state.contains("EscrowAccountInner"),
        "Pinocchio state must not emit the #[account] wrapper/inner-enum; got:\n{state}"
    );
}

/// Slice 6 step 4a — Pinocchio instruction scaffold: a struct of
/// `&AccountInfo` fields + a `process_<name>` wrapper that binds the
/// account slice positionally, LE-parses numeric params, and calls
/// `.handler()` (which calls `guards::<name>`).
#[test]
fn pinocchio_handler_scaffold_emits_struct_and_process_wrapper() {
    let src = r#"spec Vault
type Error | InvalidAmount
state { balance : U64 }
handler deposit (amount : U64) {
  accounts {
    authority : signer, writable
    vault     : writable
  }
  requires amount > 0 else InvalidAmount
  effect { balance += amount }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "deposit").unwrap();
    let out = render_pinocchio_handler_scaffold(handler, &spec).unwrap();

    // Accounts struct of &AccountInfo (no typed wrappers).
    assert!(
        out.contains("pub struct Deposit<'a> {")
            && out.contains("pub authority: &'a AccountInfo,")
            && out.contains("pub vault: &'a AccountInfo,"),
        "must emit a &AccountInfo accounts struct; got:\n{out}"
    );
    // Handler method takes the param + calls the guard.
    assert!(
        out.contains("pub fn handler(&mut self, amount: u64) -> ProgramResult {")
            && out.contains("guards::deposit(self, amount)?;"),
        "handler must take params + call guards; got:\n{out}"
    );
    // Effect body: zeropod mutable decode + checked scalar arithmetic.
    // (No MathOverflow declared → falls back to ProgramError::ArithmeticOverflow.)
    assert!(
        out.contains(
            "VaultAccount::from_bytes_mut(unsafe { self.vault.borrow_mut_data_unchecked() })"
        ),
        "effect body must mutably decode the state account; got:\n{out}"
    );
    assert!(
            out.contains("__state.balance = __state.balance.get().checked_add(amount).ok_or(ProgramError::ArithmeticOverflow)?.into();"),
            "scalar `+=` must lower to a checked .get()/.into() update; got:\n{out}"
        );
    // process_<name> wrapper: positional account binding + LE param
    // parse + struct build + dispatch.
    assert!(
            out.contains("pub fn process_deposit(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {")
                && out.contains("let [authority, vault, ..] = accounts else {")
                && out.contains("return Err(ProgramError::NotEnoughAccountKeys);")
                && out.contains("u64::from_le_bytes(")
                && out.contains("let mut ctx = Deposit { authority, vault };")
                && out.contains("ctx.handler(amount)"),
            "process wrapper must bind accounts + parse params + dispatch; got:\n{out}"
        );
    // No Anchor/Quasar Context shape.
    assert!(
        !out.contains("Context<") && !out.contains("Ctx<") && !out.contains("to_account_info"),
        "Pinocchio scaffold must not leak the Anchor/Quasar context shape; got:\n{out}"
    );
}

/// Slice 6 step 4b — Pinocchio guards.rs: signer-`auth` via `is_signer`,
/// param `requires` directly, and state-referencing `requires` via a
/// one-time zeropod decode (`State::from_bytes` + `__state.<field>.get()`).
#[test]
fn pinocchio_guards_signer_param_and_state_requires() {
    let src = r#"spec Vault
type Error | InvalidAmount | Insufficient
state { balance : U64 }
handler withdraw (amount : U64) {
  auth owner
  accounts {
    owner : signer
    vault : writable
  }
  requires amount > 0 else InvalidAmount
  requires state.balance >= amount else Insufficient
  effect { balance -= amount }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("programs");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();

    emit_pinocchio_guards(&spec, &fp, &out_dir).unwrap();
    let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();

    // Guard fn signature + pinocchio/zeropod imports.
    assert!(
        guards.contains("use zeropod::ZeroPodFixed;")
            && guards.contains("pub fn withdraw(ctx: &Withdraw, amount: u64) -> ProgramResult {"),
        "guard fn signature + imports; got:\n{guards}"
    );
    // Signer-auth.
    assert!(
        guards.contains("if !ctx.owner.is_signer() {")
            && guards.contains("return Err(ProgramError::MissingRequiredSignature);"),
        "must emit the signer-auth check; got:\n{guards}"
    );
    // Param requires — direct.
    assert!(
        guards.contains(
            "if !(amount > 0) { return Err(ProgramError::from(VaultError::InvalidAmount)); }"
        ),
        "param requires must emit a direct if-check; got:\n{guards}"
    );
    // State requires — decode + .get() on the decoded view.
    assert!(
        guards.contains("VaultAccount::from_bytes(unsafe { ctx.vault.borrow_data_unchecked() })"),
        "state-referencing requires must decode the state account; got:\n{guards}"
    );
    assert!(
        guards.contains("__state.balance.get() >= amount")
            && guards.contains("VaultError::Insufficient"),
        "state requires must read via the decoded __state view; got:\n{guards}"
    );
}

/// Regression for issue #71: `Pubkey` state fields lower to a raw
/// `[u8; 32]` in the zeropod struct (no Pod scalar wrapper), so a
/// `state.<pubkey> == <acct>.pubkey` guard must compare the field by
/// value (NO `.get()`) against the deref'd account key
/// (`*ctx.<acct>.key()`). Scalar fields keep `.get()`. The effect
/// body sets a `Pubkey` field via the deref'd value directly (no
/// `.into()`) and binds account refs through `self` (the handler
/// method), not the guard fn's `ctx`.
#[test]
fn pinocchio_pubkey_state_field_reads_by_value_not_get() {
    let src = r#"spec Cfg
program_id "11111111111111111111111111111111"
type Error | BadAuth | BadLane
type State
  | Active of { admin_key : Pubkey, lane_count : U64 }
handler set_admin : State.Active -> State.Active {
  accounts { config : writable, admin : readonly }
  effect { Active.admin_key := admin.pubkey }
}
handler check (lane_id : U64) : State.Active -> State.Active {
  accounts { config : readonly, caller : readonly }
  requires caller.pubkey == state.admin_key else BadAuth
  requires lane_id < state.lane_count else BadLane
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("programs");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    emit_pinocchio_guards(&spec, &fp, &out_dir).unwrap();
    let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();

    // Pubkey field: by value (no `.get()`), account key deref'd.
    assert!(
        guards.contains("*ctx.caller.key() == __state.admin_key"),
        "Pubkey guard must compare by value with a deref'd key; got:\n{guards}"
    );
    assert!(
        !guards.contains("admin_key.get()"),
        "Pubkey field must NOT be read through `.get()`; got:\n{guards}"
    );
    // Scalar field still reads through `.get()`.
    assert!(
        guards.contains("lane_id < __state.lane_count.get()"),
        "scalar field must keep `.get()`; got:\n{guards}"
    );

    // Effect body: Pubkey set via deref'd value, bound through `self`.
    let mut effect = String::new();
    let h = spec
        .handlers
        .iter()
        .find(|h| h.name == "set_admin")
        .unwrap();
    emit_pinocchio_effect_body(&mut effect, h, &spec);
    assert!(
        effect.contains("__state.admin_key = *self.admin.key();"),
        "Pubkey set must assign the deref'd key via `self` (no `.into()`); got:\n{effect}"
    );
}

/// v2.26 Slice 2 — the `unconstrained_modifies` lint must still
/// fire on a multi-variant ADT spec where `modifies [X]` lists a
/// field that's neither written by `effect` nor referenced by any
/// `ensures` clause. The lint is field-name-based and target-
/// agnostic, so this just locks the behavior against future
/// regressions in the ADT path.
#[test]
fn unconstrained_modifies_fires_on_adt_spec() {
    let src = r#"spec Pool

type State
  | Uninitialized
  | Active of {
      pool_balance : U64,
      lp_supply    : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle
  | Unauthorized

handler deposit (amount : U64) : State.Active -> State.Active {
  accounts {
    pool : writable
    user : signer
  }
  requires amount > 0 else MathOverflow
  modifies [pool_balance, lp_supply]
  effect {
    Active.pool_balance += amount
  }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let warnings = crate::check::check_completeness(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "unconstrained_modifies")
        .expect("unconstrained_modifies must fire on the ADT spec");
    assert!(
        hit.message.contains("'lp_supply'"),
        "lint message must name the unconstrained field; got: {}",
        hit.message
    );
}

#[test]
fn map_type_errors_on_undeclared_user_type() {
    // `Map[N] UserAccount` where UserAccount is neither a primitive nor
    // declared via `type UserAccount = …` / `type UserAccount { … }` /
    // `type UserAccount | …`. Must surface as an error naming the bad
    // inner type rather than silently emitting broken Rust.
    let spec = spec_with_constants(&[("MAX_ACCOUNTS", "8")]);
    let err = map_type("Map[MAX_ACCOUNTS] UserAccount", &spec)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("UserAccount"),
        "error should name the unsupported inner type: {err}"
    );
}

// ----- v2.8 G4: Anchor CPI codegen for SPL Token transfer -----

/// Exercise try_emit_cpi against an end-to-end-parsed spec.
/// Hits the resolver pipeline (no need to construct ParsedSpec by
/// hand) and confirms the SPL Token transfer shape lands.
#[test]
fn cpi_emits_anchor_spl_transfer_for_canonical_program_id() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable
    dst           : writable
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = n, authority = auth)
}
"#,
    )
    .unwrap();
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "send")
        .expect("send handler");
    let call = handler.calls.first().expect("call site");
    let rendered =
        try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit Anchor CPI");
    assert!(
        rendered.contains("anchor_spl::token::{self, Transfer}"),
        "must use anchor_spl::token::Transfer; got:\n{rendered}"
    );
    assert!(
        rendered.contains("from:      self.src.to_account_info()"),
        "from arg must resolve to self.src; got:\n{rendered}"
    );
    assert!(
        rendered.contains("token::transfer(CpiContext::new(cpi_program, cpi_accounts), n)"),
        "amount arg `n` is a handler param and should pass through bare; got:\n{rendered}"
    );
}

/// Helper for the Quasar / Pinocchio CPI tests — same SPL Token
/// transfer fixture shape used in
/// `cpi_emits_anchor_spl_transfer_for_canonical_program_id`, but
/// parameterized so each test can swap the called handler name in
/// the call site.
#[cfg(test)]
fn parse_spl_transfer_caller_spec(called_handler: &str) -> crate::check::ParsedSpec {
    let spec_src = format!(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {{
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {{
    discriminant "0x03"
    accounts {{
      from      : writable
      to        : writable
      authority : signer
    }}
    requires amount > 0
    ensures  amount > 0
  }}
  handler mint_to (amount : U64) {{
    discriminant "0x07"
    accounts {{
      mint      : writable
      to        : writable
      authority : signer
    }}
  }}
}}

type State | Active of {{ balance : U64 }}
type Error | E

handler send (n : U64) : State.Active -> State.Active {{
  permissionless
  accounts {{
    state         : writable
    src           : writable
    dst           : writable
    mint          : writable
    auth          : signer
    token_program : program
  }}
  call Token.{}(from = src, to = dst, mint = mint, amount = n, authority = auth)
}}
"#,
        called_handler
    );
    crate::chumsky_adapter::parse_str(&spec_src).unwrap()
}

/// Caller fixture for `mint_to`: the canonical SPL interface names the
/// signer slot `mint_authority`. Shared by the Quasar + Pinocchio
/// mint_to tests (the shared transfer fixture passes `authority`, which
/// mint_to doesn't accept).
fn parse_mint_to_caller_spec() -> ParsedSpec {
    crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler mint_to (amount : U64) {
    discriminant "0x07"
    accounts {
      mint            : writable
      to              : writable, type token
      mint_authority  : signer
    }
  }
}

type State | Active of { stash : U64 }
type Error | E

handler do_mint (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    the_mint       : writable
    holder_ta      : writable, type token
    minter         : signer
    token_program  : program
  }
  call Token.mint_to(mint = the_mint, to = holder_ta, mint_authority = minter, amount = n)
}
"#,
    )
    .unwrap()
}

/// Caller fixture for `close_account` (no scalar; account/destination/
/// authority). Shared by the Quasar + Pinocchio close tests.
fn parse_close_account_caller_spec() -> ParsedSpec {
    crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler close_account {
    discriminant "0x09"
    accounts {
      account     : writable, type token
      destination : writable
      authority   : signer
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_close : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    target_acct    : writable, type token
    sweep_target   : writable
    closer         : signer
    token_program  : program
  }
  call Token.close_account(account = target_acct, destination = sweep_target, authority = closer)
}
"#,
    )
    .unwrap()
}

/// Caller fixture for `initialize_account` (no scalar; account/mint/
/// owner/rent). Shared by the Quasar (→ None) + Pinocchio init tests.
fn parse_init_account_caller_spec() -> ParsedSpec {
    crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler initialize_account {
    discriminant "0x01"
    accounts {
      account : writable
      mint    : readonly
      owner   : readonly
      rent    : readonly
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_init : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    new_acct       : writable
    the_mint       : writable
    the_owner      : writable
    rent_sysvar    : writable
    token_program  : program
  }
  call Token.initialize_account(account = new_acct, mint = the_mint, owner = the_owner, rent = rent_sysvar)
}
"#,
        )
        .unwrap()
}

/// Spike: Quasar SPL Token transfer emits a one-line method chain
/// on the token-program account, NOT an `anchor_spl::*` builder.
/// The shape is:
///   self.token_program.transfer(&self.src, &self.dst, &self.auth, n).invoke()?;
#[test]
fn cpi_emits_quasar_spl_transfer() {
    let spec = parse_spl_transfer_caller_spec("transfer");
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered =
        try_emit_cpi(call, handler, &spec, Target::Quasar).expect("Quasar SPL transfer must emit");
    assert!(
        rendered.contains("self.token_program.transfer("),
        "Quasar shape must invoke transfer on the token-program account; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.src"),
        "from arg must resolve to &self.src; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.dst"),
        "to arg must resolve to &self.dst; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.auth"),
        "authority arg must resolve to &self.auth; got:\n{rendered}"
    );
    assert!(
        rendered.trim_end().ends_with(".invoke()?;"),
        "Quasar trait chain must terminate with .invoke()?; got:\n{rendered}"
    );
    // Anti-regression: must NOT leak the Anchor shape.
    assert!(
        !rendered.contains("anchor_spl"),
        "Quasar emission must not import anchor_spl; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("CpiContext"),
        "Quasar emission must not construct CpiContext; got:\n{rendered}"
    );
}

/// Slice 2: Quasar SPL `mint_to` emits the trait method chain. The
/// spec names the signer `mint_authority`; it resolves positionally
/// into `TokenCpi::mint_to(mint, to, authority, amount)`.
#[test]
fn cpi_emits_quasar_spl_mint_to() {
    let spec = parse_mint_to_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered =
        try_emit_cpi(call, handler, &spec, Target::Quasar).expect("Quasar SPL mint_to must emit");
    assert!(
        rendered.contains("self.token_program.mint_to("),
        "must invoke mint_to on the token-program account; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.the_mint")
            && rendered.contains("&self.holder_ta")
            && rendered.contains("&self.minter"),
        "mint/to/mint_authority must resolve to call-site accounts; got:\n{rendered}"
    );
    assert!(
        rendered.trim_end().ends_with(".invoke()?;"),
        "must terminate with .invoke()?; got:\n{rendered}"
    );
}

/// Slice 2: Quasar SPL `burn` — TokenCpi::burn(from, mint, authority,
/// amount). The shared transfer fixture supplies exactly this arg set.
#[test]
fn cpi_emits_quasar_spl_burn() {
    let spec = parse_spl_transfer_caller_spec("burn");
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered =
        try_emit_cpi(call, handler, &spec, Target::Quasar).expect("Quasar SPL burn must emit");
    assert!(
        rendered.contains("self.token_program.burn(")
            && rendered.contains("&self.src")
            && rendered.contains("&self.mint")
            && rendered.contains("&self.auth"),
        "burn must resolve from/mint/authority; got:\n{rendered}"
    );
}

/// Slice 2: Quasar SPL `close_account` — no scalar; three account args
/// (account, destination, authority).
#[test]
fn cpi_emits_quasar_spl_close_account_no_amount() {
    let spec = parse_close_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Quasar)
        .expect("Quasar SPL close_account must emit");
    assert!(
        rendered.contains("self.token_program.close_account(")
            && rendered.contains("&self.target_acct")
            && rendered.contains("&self.sweep_target")
            && rendered.contains("&self.closer"),
        "close_account must resolve account/destination/authority; got:\n{rendered}"
    );
    assert!(
        rendered.trim_end().ends_with(".invoke()?;"),
        "must terminate with .invoke()?; got:\n{rendered}"
    );
}

/// Slice 2: Quasar SPL `initialize_account` stays `None` — `quasar-spl`
/// exposes only `initialize_account3` (owner is a raw `&Address`, no
/// rent sysvar), which doesn't fit the uniform account-view helper.
#[test]
fn cpi_quasar_spl_initialize_account_falls_through_to_none() {
    let spec = parse_init_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
    let call = handler.calls.first().unwrap();
    assert!(
        try_emit_cpi(call, handler, &spec, Target::Quasar).is_none(),
        "Quasar initialize_account has no uniform shape; must fall through to None"
    );
}

/// Spike commit 2: Pinocchio SPL Token transfer emits a struct-
/// construction `Transfer { … }.invoke()?` — sibling shape to the
/// Quasar method chain but with field assignments.
///
/// Note: the Pinocchio emitter is dead code from the CLI today
/// (scaffold gate at `main.rs:3132`); this test exercises the
/// emitter directly. When slice 6 lands, this is the same string
/// the CLI emits.
#[test]
fn cpi_emits_pinocchio_spl_transfer() {
    let spec = parse_spl_transfer_caller_spec("transfer");
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio SPL transfer must emit");
    assert!(
        rendered.contains("pinocchio_token::instructions::Transfer {"),
        "Pinocchio shape must construct the qualified Transfer struct; got:\n{rendered}"
    );
    assert!(
        rendered.contains("from:") && rendered.contains("self.src"),
        "from field must resolve to self.src; got:\n{rendered}"
    );
    assert!(
        rendered.contains("to:") && rendered.contains("self.dst"),
        "to field must resolve to self.dst; got:\n{rendered}"
    );
    assert!(
        rendered.contains("authority:") && rendered.contains("self.auth"),
        "authority field must resolve to self.auth; got:\n{rendered}"
    );
    // The struct fields are `&'a AccountInfo`, so the emitter must NOT
    // prepend `&` (that would yield `&&AccountInfo`).
    assert!(
        !rendered.contains("&self."),
        "Pinocchio CPI must pass `self.<acct>` not `&self.<acct>`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("amount:"),
        "amount scalar must appear as a struct field; got:\n{rendered}"
    );
    assert!(
        rendered.contains("}.invoke()?;"),
        "Pinocchio struct must terminate with .invoke()?; got:\n{rendered}"
    );
    // Anti-regression: no Anchor / Quasar shape leakage.
    assert!(
        !rendered.contains("anchor_spl") && !rendered.contains("CpiContext"),
        "Pinocchio emission must not leak Anchor shape; got:\n{rendered}"
    );
    assert!(
        !rendered.contains(".transfer("),
        "Pinocchio is struct-construction, not method chain; got:\n{rendered}"
    );
}

/// Caller fixture for `System.transfer` (payer → pda lamport top-up).
/// Shared by the Anchor / Quasar / Pinocchio System transfer tests.
fn parse_system_transfer_caller_spec() -> ParsedSpec {
    crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface System {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    discriminant "0x02000000"
    accounts {
      from : signer, writable
      to   : writable
    }
    requires amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler topup (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    payer          : signer, writable
    pda            : writable
    system_program : program
  }
  call System.transfer(from = payer, to = pda, amount = n)
}
"#,
    )
    .unwrap()
}

/// Pinocchio System Program `transfer` CPI. `call System.transfer(...)`
/// must lower to `pinocchio_system::instructions::Transfer { from, to,
/// lamports }.invoke()?` — the spec's `amount` arg binds the struct's
/// `lamports` field. Before this slice, System calls fell through to a
/// `todo!()` breadcrumb on Pinocchio (only SPL Token was mechanized).
#[test]
fn cpi_emits_pinocchio_system_transfer() {
    let spec = parse_system_transfer_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "topup").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio System transfer must emit");
    assert!(
        rendered.contains("pinocchio_system::instructions::Transfer {"),
        "must construct the qualified System Transfer struct; got:\n{rendered}"
    );
    assert!(
        rendered.contains("from:") && rendered.contains("self.payer"),
        "from field must resolve to self.payer; got:\n{rendered}"
    );
    assert!(
        rendered.contains("to:") && rendered.contains("self.pda"),
        "to field must resolve to self.pda; got:\n{rendered}"
    );
    assert!(
        rendered.contains("lamports:"),
        "the spec `amount` arg must bind the `lamports` struct field; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("amount:"),
        "System Transfer has no `amount` field — it is `lamports`; got:\n{rendered}"
    );
    // Struct fields are `&AccountInfo`; no `&self.` double-ref.
    assert!(
        !rendered.contains("&self."),
        "Pinocchio CPI passes `self.<acct>`, not `&self.<acct>`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("}.invoke()?;"),
        "must terminate with .invoke()?; got:\n{rendered}"
    );
    // Anti-regression: no SPL-token / Anchor / Quasar shape leakage.
    assert!(
        !rendered.contains("pinocchio_token")
            && !rendered.contains("anchor")
            && !rendered.contains("CpiContext"),
        "System emission must not leak token / Anchor shape; got:\n{rendered}"
    );
}

/// Caller fixture for `System.create_account`. Shared by the
/// Quasar + Pinocchio breadcrumb tests.
fn parse_system_create_account_caller_spec() -> ParsedSpec {
    crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface System {
  program_id "11111111111111111111111111111111"
  handler create_account (lamports : U64) (space : U64) (owner : Pubkey) {
    discriminant "0x00000000"
    accounts {
      payer       : signer, writable
      new_account : signer, writable
    }
    requires lamports > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler make (l : U64) (s : U64) (o : Pubkey) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    payer          : signer, writable
    pda            : signer, writable
    system_program : program
  }
  call System.create_account(payer = payer, new_account = pda, lamports = l, space = s, owner = o)
}
"#,
    )
    .unwrap()
}

/// System `create_account` / `assign` are not mechanized this slice
/// (their `owner: &Pubkey` resolution from a spec `Pubkey` arg is a
/// follow-on). `try_emit_cpi` must return `None` so the caller keeps
/// the breadcrumb rather than emit code that may not type-check.
#[test]
fn cpi_pinocchio_system_create_account_is_breadcrumb() {
    let spec = parse_system_create_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "make").unwrap();
    let call = handler.calls.first().unwrap();
    assert!(
        try_emit_cpi(call, handler, &spec, Target::Pinocchio).is_none(),
        "create_account must stay a breadcrumb (None) this slice"
    );
}

/// Anchor System Program `transfer` CPI. `call System.transfer(...)`
/// must lower to the idiomatic `anchor_lang::system_program::transfer(
/// CpiContext::new(cpi_program, Transfer { from, to }), amount)?` shape
/// rather than the generic `solana_program::program::invoke` builder
/// (which is what System fell through to on Anchor before this slice).
#[test]
fn cpi_emits_anchor_system_transfer() {
    let spec = parse_system_transfer_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "topup").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
        .expect("Anchor System transfer must emit");
    assert!(
        rendered.contains("use anchor_lang::system_program::{self, Transfer};"),
        "must use anchor_lang::system_program::Transfer; got:\n{rendered}"
    );
    assert!(
        rendered.contains("from:") && rendered.contains("self.payer.to_account_info()"),
        "from must resolve to self.payer.to_account_info(); got:\n{rendered}"
    );
    assert!(
        rendered.contains("let cpi_program = self.system_program.to_account_info();"),
        "cpi_program must be the system_program account; got:\n{rendered}"
    );
    assert!(
        rendered
            .contains("system_program::transfer(CpiContext::new(cpi_program, cpi_accounts), n)?;"),
        "amount param `n` passes through bare to system_program::transfer; got:\n{rendered}"
    );
    // Anti-regression: must NOT fall back to the generic invoke builder
    // or leak the SPL token path.
    assert!(
        !rendered.contains("solana_program::program::invoke") && !rendered.contains("anchor_spl"),
        "System transfer must be idiomatic, not generic invoke / SPL; got:\n{rendered}"
    );
}

/// Quasar System Program `transfer` CPI. `call System.transfer(...)`
/// must lower to the `Program<SystemProgram>` method chain
/// `self.system_program.transfer(&self.payer, &self.pda, n).invoke()?;`
/// — the spec's `amount` arg binds the `lamports` positional. Before
/// this slice, ALL non-SPL Quasar CPIs fell through to the `todo!()`
/// breadcrumb.
#[test]
fn cpi_emits_quasar_system_transfer() {
    let spec = parse_system_transfer_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "topup").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Quasar)
        .expect("Quasar System transfer must emit");
    assert!(
        rendered.contains("self.system_program.transfer("),
        "must invoke transfer on the system-program account; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.payer"),
        "from arg must resolve to &self.payer; got:\n{rendered}"
    );
    assert!(
        rendered.contains("&self.pda"),
        "to arg must resolve to &self.pda; got:\n{rendered}"
    );
    assert!(
        rendered.contains(", n)"),
        "amount param `n` passes through bare as the lamports positional; got:\n{rendered}"
    );
    assert!(
        rendered.trim_end().ends_with(".invoke()?;"),
        "Quasar method chain must terminate with .invoke()?; got:\n{rendered}"
    );
    // Anti-regression: no Anchor / Pinocchio shape leakage.
    assert!(
        !rendered.contains("CpiContext")
            && !rendered.contains("anchor")
            && !rendered.contains("pinocchio_system"),
        "Quasar emission must not leak Anchor / Pinocchio shape; got:\n{rendered}"
    );
}

/// Quasar System `create_account` / `assign` are not mechanized this
/// slice (same `&Address` owner-resolution follow-on as the Anchor /
/// Pinocchio emitters). `try_emit_cpi` must return `None` so the
/// caller keeps the breadcrumb.
#[test]
fn cpi_quasar_system_create_account_is_breadcrumb() {
    let spec = parse_system_create_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "make").unwrap();
    let call = handler.calls.first().unwrap();
    assert!(
        try_emit_cpi(call, handler, &spec, Target::Quasar).is_none(),
        "Quasar create_account must stay a breadcrumb (None) this slice"
    );
}

/// Slice 2b: Pinocchio SPL `mint_to` constructs the MintTo struct.
/// pinocchio-token names the recipient slot `account` (canonical SPL
/// `to`) and the signer `mint_authority`.
#[test]
fn cpi_emits_pinocchio_spl_mint_to() {
    let spec = parse_mint_to_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio SPL mint_to must emit");
    assert!(
        rendered.contains("pinocchio_token::instructions::MintTo {"),
        "must construct the MintTo struct; got:\n{rendered}"
    );
    assert!(
        rendered.contains("mint:") && rendered.contains("self.the_mint"),
        "mint field; got:\n{rendered}"
    );
    assert!(
        rendered.contains("account:") && rendered.contains("self.holder_ta"),
        "recipient maps to pinocchio field `account` ← spec `to`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("mint_authority:") && rendered.contains("self.minter"),
        "signer maps to `mint_authority`; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("&self."),
        "Pinocchio CPI must pass `self.<acct>` not `&self.<acct>`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
        "amount scalar + .invoke()?; got:\n{rendered}"
    );
}

/// Slice 2b: Pinocchio SPL `burn` — Burn names the source slot
/// `account` (canonical SPL `from`).
#[test]
fn cpi_emits_pinocchio_spl_burn() {
    let spec = parse_spl_transfer_caller_spec("burn");
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio SPL burn must emit");
    assert!(
        rendered.contains("pinocchio_token::instructions::Burn {"),
        "must construct the Burn struct; got:\n{rendered}"
    );
    assert!(
        rendered.contains("account:") && rendered.contains("self.src"),
        "source maps to pinocchio field `account` ← spec `from`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("mint:") && rendered.contains("authority:"),
        "mint + authority fields; got:\n{rendered}"
    );
}

/// Slice 2b: Pinocchio SPL `initialize_account` — no scalar; rent
/// sysvar maps to pinocchio field `rent_sysvar` (canonical SPL `rent`).
#[test]
fn cpi_emits_pinocchio_spl_initialize_account_no_amount() {
    let spec = parse_init_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio SPL initialize_account must emit");
    assert!(
        rendered.contains("pinocchio_token::instructions::InitializeAccount {"),
        "must construct InitializeAccount; got:\n{rendered}"
    );
    assert!(
        rendered.contains("rent_sysvar:") && rendered.contains("self.rent_sysvar"),
        "rent maps to pinocchio field `rent_sysvar`; got:\n{rendered}"
    );
    // No scalar — no `amount:` field.
    assert!(
        !rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
        "no-amount handler must not emit an amount field; got:\n{rendered}"
    );
}

/// Slice 2b: Pinocchio SPL `close_account` — no scalar.
#[test]
fn cpi_emits_pinocchio_spl_close_account_no_amount() {
    let spec = parse_close_account_caller_spec();
    let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
        .expect("Pinocchio SPL close_account must emit");
    assert!(
        rendered.contains("pinocchio_token::instructions::CloseAccount {")
            && rendered.contains("self.target_acct")
            && rendered.contains("self.sweep_target")
            && rendered.contains("self.closer"),
        "close_account must resolve account/destination/authority; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
        "no-amount handler must not emit an amount field; got:\n{rendered}"
    );
}

/// Pinocchio generic (non-SPL) CPI is unimplemented; the
/// `(Target::Pinocchio, false)` branch in `try_emit_cpi` returns
/// None (generic non-SPL Pinocchio CPI is a follow-on slice).
#[test]
fn cpi_pinocchio_non_spl_falls_through_to_none() {
    // A spec whose called interface is NOT the SPL Token program.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts { src : writable }
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src : writable
  }
  call MyAmm.swap(src = src, amount = balance)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    assert!(
        try_emit_cpi(call, handler, &spec, Target::Pinocchio).is_none(),
        "Pinocchio generic CPI is unimplemented; must fall through to None"
    );
}

#[test]
fn anchor_sighash_matches_known_discriminators() {
    // Anchor's discriminator = sha256("global:<handler>")[..8].
    // Verify the function uses the right input format by computing
    // the expected value via sha2 directly, confirming both prefix
    // and slice-length are correct. If `anchor_sighash` ever drifts
    // (e.g. wrong prefix, different hash, wrong slice), this test
    // catches it independently of what value the function produces.
    use sha2::{Digest, Sha256};
    for handler in ["initialize", "transfer", "swap", "do_nothing"] {
        let mut hasher = Sha256::new();
        hasher.update(format!("global:{}", handler).as_bytes());
        let full = hasher.finalize();
        let mut expected = [0u8; 8];
        expected.copy_from_slice(&full[..8]);
        assert_eq!(
            anchor_sighash(handler),
            expected,
            "sighash for `{handler}` should be sha256(\"global:{handler}\")[..8]"
        );
    }
    // Sanity: different handler names produce different sighashes.
    assert_ne!(anchor_sighash("a"), anchor_sighash("b"));
}

#[test]
fn to_snake_case_handles_pascal_and_camel() {
    assert_eq!(to_snake_case("MyAmm"), "my_amm");
    assert_eq!(to_snake_case("SPLToken"), "s_p_l_token");
    assert_eq!(to_snake_case("Token"), "token");
    assert_eq!(to_snake_case("simple"), "simple");
    assert_eq!(to_snake_case("FooBarBaz"), "foo_bar_baz");
}

#[test]
fn cpi_generic_returns_none_when_program_account_is_missing() {
    // No `<iface>_program` account, no unique non-token-program
    // account either. Caller falls back to comment + todo!().
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts { src : writable }
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src : writable
  }
  call MyAmm.swap(src = src, amount = balance)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    assert!(
        try_emit_cpi(call, handler, &spec, Target::Anchor).is_none(),
        "missing program account should defer to comment + todo!()"
    );
}

#[test]
fn cpi_emits_generic_invoke_shape_for_non_spl_token_interface() {
    // v2.9 G3: non-SPL-Token interfaces get the generic
    // `solana_program::program::invoke` shape rather than v2.8's
    // None / comment-only fallback.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts {
      src : writable
      dst : writable
    }
    ensures amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src          : writable
    dst          : writable
    my_amm_program : program
  }
  call MyAmm.swap(src = src, dst = dst, amount = balance)
}
"#,
    )
    .unwrap();
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "send")
        .expect("send handler");
    let call = handler.calls.first().expect("call site");
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
        .expect("must emit a generic CPI shape for non-SPL Anchor programs");

    // Sanity-check the emitted shape:
    assert!(rendered.contains("solana_program::program::invoke"));
    assert!(rendered.contains("Instruction"));
    assert!(rendered.contains("AccountMeta::new(self.src.key(), false)"));
    assert!(rendered.contains("AccountMeta::new(self.dst.key(), false)"));
    // The program account ends up in the AccountInfo array passed to
    // invoke (so the runtime can validate it).
    assert!(rendered.contains("self.my_amm_program.to_account_info()"));
    // Discriminator: first byte of sha256("global:swap") is 0xf8.
    assert!(
        rendered.contains("0xf8"),
        "expected sighash for `swap` to start with 0xf8; got:\n{rendered}"
    );
    // Borsh-serialized amount arg.
    assert!(rendered.contains("AnchorSerialize::serialize"));
}

/// v2.24 #11 — `let X = call Foo.handler(...)` lowers to a Rust
/// let-binding capturing the callee's return value via Solana's
/// `get_return_data` syscall, when the interface declares a
/// return type (`-> U64` etc.).
#[test]
fn cpi_emits_let_binding_when_interface_declares_return_type() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "22222222222222222222222222222222"
  handler absorb_loss (loss : U64) -> U64 {
    accounts { vault : writable }
  }
}

type State | Active of { total_burned : U64 }
type Error | MathOverflow | E

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    vault         : writable
    pool_program  : program
  }
  let burned = call Pool.absorb_loss(vault = vault, loss = loss)
}
"#,
    )
    .unwrap();
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "liquidate")
        .unwrap();
    let call = handler.calls.first().expect("call site");
    assert_eq!(
        call.result_binding.as_deref(),
        Some("burned"),
        "result_binding should land in ParsedCall"
    );
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
        .expect("must emit a generic CPI with let-binding capture");
    // Block opens as a `let burned = { … }` expression.
    assert!(
        rendered.starts_with("        let burned = {\n"),
        "expected let-binding open; got prefix:\n{}",
        &rendered[..200.min(rendered.len())]
    );
    // The CPI invoke happens inside.
    assert!(rendered.contains("invoke(&ix"));
    // get_return_data captures the callee's return.
    assert!(rendered.contains("get_return_data"));
    // The return type maps to u64; deserialize is typed.
    assert!(
        rendered.contains("<u64 as AnchorDeserialize>"),
        "expected typed deserialize for U64 return; got:\n{rendered}"
    );
    // Block closes with `};` (let-binding terminator).
    assert!(
        rendered.ends_with("        };\n"),
        "expected let-binding close; got suffix:\n{}",
        &rendered[rendered.len().saturating_sub(200)..]
    );
}

// ----- v2.8 F8: Error-sum threading via mechanize_effect -----

#[test]
fn mechanize_effect_references_program_error_enum_for_checked_add() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec MyProgram
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow

handler bump (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state : writable
  }
  effect { pool += n }
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "bump").unwrap();
    let state_acct = find_state_account(handler).expect("state account");
    let effect = handler.effects.first().unwrap();
    let rendered =
        mechanize_effect(effect, state_acct, handler, &spec, Target::Anchor).expect("mechanized");
    // Pre-F8 this said `ErrorCode::MathOverflow` (a non-existent enum).
    // F8: it now says `<ProgramName>Error::MathOverflow`, matching the
    // user's declared Error sum.
    assert!(
        rendered.contains("MyProgramError::MathOverflow"),
        "expected program-specific Error enum; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("ErrorCode::MathOverflow"),
        "should not reference the legacy non-existent ErrorCode enum; got:\n{rendered}"
    );
}

// ----- v2.24 §S1a/b/c: per-site override + pragma + underflow default -----

fn mechanize_first_effect(src: &str, handler_name: &str) -> String {
    let spec = crate::chumsky_adapter::parse_str(src).unwrap();
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == handler_name)
        .expect("handler not found");
    let state_acct = find_state_account(handler).expect("state account");
    let effect = handler.effects.first().expect("at least one effect");
    mechanize_effect(effect, state_acct, handler, &spec, Target::Anchor).expect("mechanized")
}

#[test]
fn per_site_else_overrides_default_variant_for_checked_add() {
    let rendered = mechanize_first_effect(
        r#"spec Mint
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow | MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { pool += n else MintOverflow }
}
"#,
        "deposit",
    );
    assert!(
        rendered.contains("MintError::MintOverflow"),
        "v2.24 §S1a: `else MintOverflow` should lower to MintError::MintOverflow; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("MintError::MathOverflow"),
        "should not use the default; got:\n{rendered}"
    );
}

#[test]
fn pragma_overrides_default_variant_when_no_per_site_override() {
    let rendered = mechanize_first_effect(
        r#"spec Mint
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow | MintOverflow

pragma checked_overflow_error = MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { pool += n }
}
"#,
        "deposit",
    );
    assert!(
        rendered.contains("MintError::MintOverflow"),
        "v2.24 §S1b: pragma checked_overflow_error should override the default; got:\n{rendered}"
    );
}

#[test]
fn checked_sub_defaults_to_math_underflow_when_declared() {
    let rendered = mechanize_first_effect(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { balance -= n }
}
"#,
        "withdraw",
    );
    assert!(
        rendered.contains("PoolError::MathUnderflow"),
        "v2.24 §S1c: -= should default to MathUnderflow when declared; got:\n{rendered}"
    );
}

#[test]
fn checked_sub_falls_back_to_math_overflow_for_legacy_specs() {
    // S1c back-compat: only MathOverflow declared, no MathUnderflow.
    // `-=` keeps raising MathOverflow (pre-v2.24 behavior) so existing
    // specs continue to build without spec edits.
    let rendered = mechanize_first_effect(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { balance -= n }
}
"#,
        "withdraw",
    );
    assert!(
        rendered.contains("PoolError::MathOverflow"),
        "v2.24 §S1c back-compat: legacy spec falls back to MathOverflow; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("MathUnderflow"),
        "back-compat path should not reference MathUnderflow; got:\n{rendered}"
    );
}

#[test]
fn cpi_emits_anchor_spl_mint_to_with_authority_renaming() {
    // Spec exposes `mint_authority` per SPL Token docs; anchor_spl's
    // MintTo struct calls the same slot `authority`. The codegen
    // boundary maps the names.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler mint_to (amount : U64) {
    discriminant "0x07"
    accounts {
      mint            : writable
      to              : writable, type token
      mint_authority  : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler do_mint (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    the_mint       : writable
    holder_ta      : writable, type token
    minter         : signer
    token_program  : program
  }
  call Token.mint_to(mint = the_mint, to = holder_ta, mint_authority = minter, amount = n)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
    assert!(
        rendered.contains("anchor_spl::token::{self, MintTo}"),
        "should use MintTo struct; got:\n{rendered}"
    );
    // anchor_spl uses `authority`; spec uses `mint_authority` — the
    // mapping should land the call-site `minter` value at the
    // `authority` field. Padding may insert extra whitespace before
    // `self`, so we check the substring on each side independently.
    assert!(
        rendered.contains("self.minter.to_account_info()"),
        "minter should be wired into the cpi_accounts struct; got:\n{rendered}"
    );
    assert!(
        rendered.contains("authority:"),
        "MintTo struct should use field name `authority`; got:\n{rendered}"
    );
    assert!(
        rendered.contains("token::mint_to(CpiContext::new(cpi_program, cpi_accounts), n)"),
        "should invoke token::mint_to with the amount; got:\n{rendered}"
    );
}

#[test]
fn cpi_emits_anchor_spl_burn() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler burn (amount : U64) {
    discriminant "0x08"
    accounts {
      from      : writable, type token
      mint      : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_burn (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    holder_ta      : writable, type token
    the_mint       : writable
    holder         : signer
    token_program  : program
  }
  call Token.burn(from = holder_ta, mint = the_mint, authority = holder, amount = n)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "do_burn").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
    assert!(rendered.contains("anchor_spl::token::{self, Burn}"));
    assert!(rendered.contains("token::burn(CpiContext::new"));
    // Padding aligns colons across fields; use a substring that's
    // independent of whitespace.
    assert!(
        rendered.contains("self.holder_ta.to_account_info()"),
        "burn's `from` should resolve to self.holder_ta; got:\n{rendered}"
    );
    assert!(rendered.contains("authority: self.holder.to_account_info()"));
}

#[test]
fn cpi_emits_anchor_spl_initialize_account_no_amount() {
    let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler initialize_account {
    discriminant "0x01"
    accounts {
      account : writable
      mint    : readonly
      owner   : readonly
      rent    : readonly
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_init : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    new_acct       : writable
    the_mint       : writable
    the_owner      : writable
    rent_sysvar    : writable
    token_program  : program
  }
  call Token.initialize_account(account = new_acct, mint = the_mint, owner = the_owner, rent = rent_sysvar)
}
"#,
        )
        .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
    assert!(rendered.contains("InitializeAccount"));
    // No scalar arg — the invocation has no second positional parameter.
    assert!(
        rendered
            .contains("token::initialize_account(CpiContext::new(cpi_program, cpi_accounts))?;"),
        "no-amount handler should not get a trailing argument; got:\n{rendered}"
    );
    // Owner-as-authority renaming.
    assert!(
        rendered.contains("self.the_owner.to_account_info()"),
        "the_owner should be wired in; got:\n{rendered}"
    );
    assert!(
        rendered.contains("authority:"),
        "InitializeAccount should use field name `authority` for the owner slot; got:\n{rendered}"
    );
}

#[test]
fn cpi_emits_anchor_spl_close_account_no_amount() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler close_account {
    discriminant "0x09"
    accounts {
      account     : writable, type token
      destination : writable
      authority   : signer
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_close : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    target_acct    : writable, type token
    sweep_target   : writable
    closer         : signer
    token_program  : program
  }
  call Token.close_account(account = target_acct, destination = sweep_target, authority = closer)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
    assert!(rendered.contains("CloseAccount"));
    assert!(rendered.contains("token::close_account(CpiContext::new(cpi_program, cpi_accounts))?;"));
}

#[test]
fn cpi_resolves_state_field_amount_to_self_state_field() {
    // The amount arg references a state field — the emitted code should
    // bind it as self.<state_acct>.<field>, not bare.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable, type token
    dst           : writable, type token
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = stash, authority = auth)
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
    let call = handler.calls.first().unwrap();
    let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
    assert!(
        rendered.contains("self.state.stash"),
        "state-field amount must resolve to self.<state_acct>.<field>; got:\n{rendered}"
    );
}

// ── S2.3: Cargo.toml section + dep preservation ───────────────────────

#[test]
fn parse_toml_sections_splits_correctly() {
    let toml = r#"# preamble

[package]
name = "foo"

[dependencies]
anchor-lang = "0.30"

[dev-dependencies]
proptest = "1"
"#;
    let parsed = parse_toml_sections(toml);
    assert!(parsed.preamble.contains("preamble"));
    assert_eq!(parsed.sections.len(), 3);
    assert_eq!(parsed.sections[0].0, "package");
    assert!(parsed.sections[0].1.contains("name = \"foo\""));
    assert_eq!(parsed.sections[1].0, "dependencies");
    assert_eq!(parsed.sections[2].0, "dev-dependencies");
}

#[test]
fn merge_cargo_toml_preserves_user_sections() {
    let existing = r#"# generated by qedgen older spec-hash

[package]
name = "user-renamed"
version = "0.2.0"
edition = "2021"

[dependencies]
anchor-lang = "0.30"
anyhow = "1"

[dev-dependencies]
proptest = "1"

[profile.release]
opt-level = 3
"#;
    let fresh = r#"# ---- GENERATED BY QEDGEN ---- spec-hash:abc123

[package]
name = "buggy"
version = "0.1.0"
edition = "2021"

[dependencies]
anchor-lang = "0.32.1"
qedgen-macros = { git = "https://example.com" }

[workspace]
"#;
    let merged = merge_cargo_toml(existing, fresh);
    // Preamble comes from fresh (qedgen marker).
    assert!(merged.contains("GENERATED BY QEDGEN"));
    // qedgen-owned `[package]` is fully replaced — user's renamed
    // `name` is overwritten back to the spec's program name. (PRD
    // trade-off: `[package]` is qedgen-owned; users wanting a
    // different crate name should change the spec's `program_name`.)
    assert!(merged.contains("name = \"buggy\""));
    // qedgen-managed deps are upserted.
    assert!(merged.contains("anchor-lang = \"0.32.1\""));
    assert!(merged.contains("qedgen-macros"));
    // User-added `anyhow` dep is preserved.
    assert!(merged.contains("anyhow = \"1\""), "got:\n{merged}");
    // User-added sections are preserved verbatim.
    assert!(merged.contains("[dev-dependencies]"));
    assert!(merged.contains("proptest = \"1\""));
    assert!(merged.contains("[profile.release]"));
    assert!(merged.contains("opt-level = 3"));
}

#[test]
fn merge_cargo_toml_handles_greenfield_existing() {
    // Existing file has no qedgen sections — merge should still
    // produce a working file (qedgen sections appended).
    let existing = r#"[dev-dependencies]
proptest = "1"
"#;
    let fresh = r#"# ---- GENERATED BY QEDGEN ----

[package]
name = "foo"

[dependencies]
anchor-lang = "0.32"

[workspace]
"#;
    let merged = merge_cargo_toml(existing, fresh);
    assert!(merged.contains("[dev-dependencies]"));
    assert!(merged.contains("[package]"));
    assert!(merged.contains("[dependencies]"));
    assert!(merged.contains("[workspace]"));
}

/// v2.29 Slice C — payload-pre + payload-post cross-variant
/// promotion emits a destructure preamble that captures the
/// referenced pre fields as local bindings, followed by the
/// post-variant assignment that reads those bindings.
#[test]
fn cross_variant_promotion_payload_to_payload_emits_destructure() {
    let src = r#"spec Promote
program_id "11111111111111111111111111111111"

type State
  | Open of { x : U64, y : U64 }
  | Closed of { y : U64 }

type Error
  | WrongState

handler close : State.Open -> State.Closed {
  accounts {
    authority : signer
    state_acct : writable
  }
  effect {
    state := .Closed { y := state.x }
  }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "close")
        .expect("close handler");
    let acct = spec.account_types.first().expect("state account type");
    let post_variant = acct
        .variants
        .iter()
        .find(|v| v.name == "Closed")
        .expect("Closed variant");
    let body = emit_cross_variant_promotion(
        handler,
        &spec,
        "state_acct",
        "Open",
        post_variant,
        "PromoteAccountInner",
        "PromoteError",
    )
    .expect("payload->payload promotion should lower in v2.29");
    // The destructure should bind `x` (referenced by `y := state.x`),
    // ignore `y` (post writes it from `x`, doesn't read pre's y),
    // and capture via `.clone()` so non-Copy variant fields work.
    assert!(
        body.contains("let x = match &self.state_acct.inner"),
        "expected destructure preamble binding `x`; got:\n{body}"
    );
    assert!(
        body.contains("PromoteAccountInner::Open { x, .. } => x.clone()"),
        "expected single-binding match arm with .clone(); got:\n{body}"
    );
    assert!(
        body.contains("return Err(PromoteError::WrongState.into())"),
        "expected WrongState guard on the no-match arm; got:\n{body}"
    );
    assert!(
        body.contains("self.state_acct.inner = PromoteAccountInner::Closed {"),
        "expected the post-variant assignment; got:\n{body}"
    );
    assert!(
        body.contains("y: x,"),
        "expected `y: x,` referencing the destructured local; got:\n{body}"
    );
}

/// v2.29 Slice H — when a spec's `imported_namespaces` carries an
/// account type, codegen emits `src/imported/<ns>.rs` with the
/// mirrored struct plus a `src/imported/mod.rs` re-exporter, and
/// `src/lib.rs` declares `pub mod imported;`. Bundled-stub-only
/// imports leave the map empty and the mirror dir is never
/// created.
#[test]
fn imported_namespace_emits_local_mirror() {
    let mut spec = ParsedSpec {
        program_name: "ConsumerProgram".into(),
        ..ParsedSpec::default()
    };
    spec.account_types.push(ParsedAccountType {
        name: "Consumer".into(),
        fields: vec![("balance".into(), "U64".into())],
        lifecycle: vec![],
        pda_ref: None,
        variants: vec![],
    });
    // Inject an imported namespace by hand (the resolver path is
    // exercised by check.rs tests; this test focuses on the
    // codegen-side mirror emission).
    let mut imported = ImportedNamespace {
        dep_key: "foreign_dep".into(),
        account_types: vec![ParsedAccountType {
            name: "ForeignState".into(),
            fields: vec![
                ("admin".into(), "Pubkey".into()),
                ("counter".into(), "U64".into()),
            ],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        }],
        records: vec![],
    };
    let _ = &mut imported;
    spec.imported_namespaces.insert("Foreign".into(), imported);

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("programs");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();

    generate_imported_mirror(&spec, &fp, &out_dir, Target::Anchor)
        .expect("imported mirror generation should succeed");

    let ns_file = out_dir.join("src/imported/Foreign.rs");
    let body = std::fs::read_to_string(&ns_file).expect("namespace mirror file should be written");
    assert!(
        body.contains("pub struct ForeignState"),
        "expected `ForeignState` mirror struct; got:\n{body}"
    );
    assert!(
        body.contains("pub admin: Pubkey,"),
        "expected `admin: Pubkey` field; got:\n{body}"
    );
    assert!(
        body.contains("#[account]"),
        "expected `#[account]` attr (Anchor target); got:\n{body}"
    );

    let mod_file = out_dir.join("src/imported/mod.rs");
    let mod_body = std::fs::read_to_string(&mod_file).expect("imported mod.rs should be written");
    assert!(
        mod_body.contains("pub mod Foreign;"),
        "expected `pub mod Foreign;` re-export; got:\n{mod_body}"
    );
}

/// v2.29 Slice H — multi-variant imported account types lower to
/// the wrapper-struct + inner-enum shape and emit accessor
/// methods on the inner enum (mirrors `generate_state`'s Slice B
/// accessor work).
#[test]
fn imported_multi_variant_namespace_emits_accessors() {
    let mut spec = ParsedSpec {
        program_name: "Consumer".into(),
        ..ParsedSpec::default()
    };
    spec.account_types.push(ParsedAccountType {
        name: "Local".into(),
        fields: vec![("x".into(), "U64".into())],
        lifecycle: vec![],
        pda_ref: None,
        variants: vec![],
    });
    let imported = ImportedNamespace {
        dep_key: "amm_dep".into(),
        account_types: vec![ParsedAccountType {
            name: "Pool".into(),
            fields: vec![],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![
                ParsedVariant {
                    name: "Open".into(),
                    fields: vec![
                        ("admin".into(), "Pubkey".into()),
                        ("balance".into(), "U64".into()),
                    ],
                },
                ParsedVariant {
                    name: "Closed".into(),
                    fields: vec![("admin".into(), "Pubkey".into())],
                },
            ],
        }],
        records: vec![],
    };
    spec.imported_namespaces.insert("AMM".into(), imported);

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("programs");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();

    generate_imported_mirror(&spec, &fp, &out_dir, Target::Anchor)
        .expect("imported mirror generation should succeed");

    let body = std::fs::read_to_string(out_dir.join("src/imported/AMM.rs"))
        .expect("AMM mirror file should be written");
    assert!(
        body.contains("pub struct Pool"),
        "expected wrapper struct; got:\n{body}"
    );
    assert!(
        body.contains("pub inner: PoolInner,"),
        "expected `inner: PoolInner` field; got:\n{body}"
    );
    assert!(
        body.contains("pub enum PoolInner"),
        "expected inner enum; got:\n{body}"
    );
    // `admin` exists in both variants — accessor emitted, no
    // panic arm because the match exhausts.
    assert!(
        body.contains("pub fn admin(&self) -> &Pubkey"),
        "expected `admin` accessor; got:\n{body}"
    );
    // `balance` only in Open — accessor emits with a panic arm.
    assert!(
        body.contains("pub fn balance(&self) -> &u64"),
        "expected `balance` accessor; got:\n{body}"
    );
    assert!(
        body.contains("PoolInner::balance() called on a variant without `balance`"),
        "expected panic message for missing variant; got:\n{body}"
    );
}

/// v2.29.2 — `rewrite_state_refs_for_self` must work on handlers
/// whose accounts block has multiple writable candidates and no
/// PDA / `on_account` disambiguator (real-world specs frequently
/// mark the state account `readonly` in handlers that only read
/// it, so the per-handler resolver returns None). The spec-wide
/// canonical-state heuristic picks the account name that's
/// writable in the most other handlers and uses it as the binder
/// target, even when this handler declares it `readonly`.
#[test]
fn rewrite_state_refs_uses_canonical_fallback_when_handler_state_acct_is_readonly() {
    const SRC: &str = r#"
spec Pool

type State
  | Active of {
      balance : U64,
    }

type Error | E

handler init (initial : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    user        : signer
    pool_config : writable
  }
  effect { balance := initial }
}

handler read_via_writable_decoy (amt : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    user        : signer
    pool_config : readonly
    decoy_a     : writable
    decoy_b     : writable
  }
  requires amt <= state.balance else E
  effect { balance := balance }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(SRC).expect("fixture should parse");
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "read_via_writable_decoy")
        .expect("handler not found");
    // `find_state_account` alone returns None for this handler (two
    // writable candidates: decoy_a, decoy_b — neither is the state
    // account; pool_config is readonly so it's only included in the
    // require_writable=false fallback, which then yields multiple
    // candidates too).
    assert!(
        find_state_account(handler).is_none(),
        "pre-condition: this fixture must surface the canonical-fallback path"
    );
    // The canonical fallback picks `pool_config` (writable in
    // `init`) and the rewriter routes `s.balance` through
    // `self.pool_config.balance`.
    let rewritten = rewrite_state_refs_for_self("s.balance + 1", handler, &spec);
    assert_eq!(
        rewritten, "self.pool_config.balance + 1",
        "v2.29.2 canonical-fallback rewrite must produce \
             `self.pool_config.balance` even when pool_config is \
             readonly in this handler; got: `{rewritten}`"
    );
}

// ----- #151 Slice 3: structural "is simple" gate for the mechanize paths -----

#[test]
fn tree_bare_rhs_admits_exactly_the_legacy_whitelist_shapes() {
    use crate::mir::expr_tree::{BindingKind, ExprTree, TreeArithOp, TreePath, TreeSeg};
    let path = |root: &str, binding: BindingKind, segments: Vec<TreeSeg>| {
        ExprTree::Path(TreePath {
            root: root.into(),
            binding,
            segments,
            ty: None,
        })
    };
    // Literals and bare params render verbatim.
    assert_eq!(tree_bare_rhs(&ExprTree::Int(42)).as_deref(), Some("42"));
    assert_eq!(
        tree_bare_rhs(&ExprTree::Bool(true)).as_deref(),
        Some("true")
    );
    assert_eq!(
        tree_bare_rhs(&path("amount", BindingKind::Param, vec![])).as_deref(),
        Some("amount")
    );
    // Consts substitute their resolved value (mirrors `resolve_value`).
    assert_eq!(
        tree_bare_rhs(&path("LIMIT", BindingKind::Const("100".into()), vec![])).as_deref(),
        Some("100")
    );
    // Single state-field reads render bare — callers add the receiver.
    assert_eq!(
        tree_bare_rhs(&path(
            "state",
            BindingKind::StateField,
            vec![TreeSeg::Field("rfp_buyer".into())]
        ))
        .as_deref(),
        Some("rfp_buyer")
    );
    // Indexed / nested state reads and compound shapes stay agent-fill.
    assert_eq!(
        tree_bare_rhs(&path(
            "state",
            BindingKind::StateField,
            vec![TreeSeg::Field("voted".into()), TreeSeg::Index("i".into())]
        )),
        None
    );
    assert_eq!(
        tree_bare_rhs(&path(
            "buyer",
            BindingKind::Account,
            vec![TreeSeg::Field("pubkey".into())]
        )),
        None
    );
    assert_eq!(
        tree_bare_rhs(&ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(path("pool", BindingKind::StateField, vec![])),
            rhs: Box::new(path("amount", BindingKind::Param, vec![])),
        }),
        None
    );
}

#[test]
fn mechanize_effect_tree_path_binds_state_field_rhs_through_receiver() {
    // `bid_buyer := state.rfp_buyer` — the tree path must produce the same
    // `self.<acct>.<field>` receiver the legacy `resolve_value` surgery did.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Rfp
program_id "11111111111111111111111111111111"
type State | Active of { rfp_buyer : Pubkey, bid_buyer : Pubkey }

handler set_bid : State.Active -> State.Active {
  permissionless
  accounts {
    state : writable
  }
  effect { bid_buyer := state.rfp_buyer }
}
"#,
    )
    .unwrap();
    let handler = spec.handlers.iter().find(|h| h.name == "set_bid").unwrap();
    let state_acct = find_state_account(handler).expect("state account");
    let effect = handler.effects.first().unwrap();
    assert!(
        effect.tree.is_some(),
        "adapter must carry a tree for this RHS"
    );
    let rendered =
        mechanize_effect(effect, state_acct, handler, &spec, Target::Anchor).expect("mechanized");
    assert_eq!(
        rendered,
        "        self.state.bid_buyer = self.state.rfp_buyer;\n"
    );
}
