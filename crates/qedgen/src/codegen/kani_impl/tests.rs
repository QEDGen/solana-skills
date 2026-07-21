use super::*;
use crate::chumsky_adapter::parse_str;

/// Run a test body on a thread with a large stack. A couple of the
/// nested-container brownfield specs below lower a deeply-recursive ensures
/// (e.g. `Option<Hook{ Vec<Con{Kind}> }>` under a nested `match`/`exists`), and
/// the unoptimized recursion overflows the default 2 MB test-thread stack on
/// some platforms (macOS in particular). This is a test-harness stack budget,
/// NOT a codegen defect: the generator terminates correctly — release builds
/// and a larger stack both pass. `resume_unwind` re-raises the original panic
/// so assertion failures still surface with their real message.
fn with_big_stack(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .expect("spawn test thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

/// A conservation ensures (`post.X == pre.X + delta`) must NOT classify
/// `X` as unchanged: the rendered string contains `post.X==pre.X` as a
/// substring, and an unanchored match emits an equality assertion that
/// fails on a correct implementation.
#[test]
fn unchanged_fields_exclude_pre_plus_delta_ensures() {
    assert_eq!(
        pinocchio_unchanged_ensures_fields(
            "post.fee_pool == pre.fee_pool + fee && post.admin == pre.admin"
        ),
        vec!["admin".to_string()],
    );
    // Reversed orientation continues the same way.
    assert_eq!(
        pinocchio_unchanged_ensures_fields("pre.fee_pool == post.fee_pool - fee"),
        Vec::<String>::new(),
    );
    // Genuinely-unchanged claims still match at every anchored position:
    // end of expression, before `&&`, and inside parens.
    assert_eq!(
        pinocchio_unchanged_ensures_fields("(post.vault == pre.vault) && post.total == pre.total"),
        vec!["total".to_string(), "vault".to_string()],
    );
}

/// Auto-trigger fires when a handler has `modifies` listing a field
/// that's absent from the effect block's LHS set (the LP-deposit
/// shape).
#[test]
fn auto_trigger_fires_on_lp_shape() {
    let src = r#"spec Pool
state { pool_balance : U64, lp_supply : U64 }
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  ensures state.pool_balance == old(state.pool_balance) + amount
  effect {
    pool_balance += amount
  }
}"#;
    let spec = parse_str(src).expect("parse");
    let h = &spec.handlers[0];
    assert!(
        handler_triggers_impl_harness(h),
        "modifies = [pool_balance, lp_supply] but effect only writes pool_balance → trigger",
    );
    assert!(spec_triggers_impl_harness(&spec));
}

/// Auto-trigger does NOT fire when modifies matches the effect-LHS
/// set (no LP-shape gap — the spec's effect block covers every
/// declared write).
#[test]
fn auto_trigger_silent_when_modifies_matches_effects() {
    let src = r#"spec Counter
state { count : U64 }
handler bump (delta : U64) {
  requires delta > 0 else InvalidAmount
  modifies [count]
  ensures state.count == old(state.count) + delta
  effect {
    count += delta
  }
}"#;
    let spec = parse_str(src).expect("parse");
    let h = &spec.handlers[0];
    assert!(
        !handler_triggers_impl_harness(h),
        "modifies = [count] = effect LHS = {{count}} → no trigger",
    );
    assert!(!spec_triggers_impl_harness(&spec));
}

/// Auto-trigger silent when no `modifies` clause is declared at all.
/// Bundled examples today take this path.
#[test]
fn auto_trigger_silent_without_modifies() {
    let src = r#"spec NoModifies
state { x : U64 }
handler set_x (v : U64) {
  ensures state.x == v
  effect { x := v }
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(!spec_triggers_impl_harness(&spec));
}

/// Slice 5: Quasar emits the struct-based impl harness, reusing the
/// Anchor symbolic-accounts builder + per-handler proof emitter (the
/// Quasar scaffold's `Ctx<X>` dispatcher forwards to the same
/// `impl <Pascal> { fn handler(&mut self, …) }` method). The header
/// must be Quasar-flavored and must NOT leak the Anchor framework
/// crates or the Pinocchio stack scaffold.
#[test]
fn quasar_target_emits_handler_harness() {
    let src = r#"spec QuasarBump
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_quasar_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Quasar)
        .expect("Quasar kani_impl must emit");
    assert!(tmp.is_file(), "Quasar target must write a harness file");
    let body = std::fs::read_to_string(&tmp).unwrap();

    // Quasar-flavored header.
    assert!(
        compact(&body).contains(&compact("Quasar (`#[program]`) program")),
        "header must name the Quasar framework; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("#[cfg(kani)] mod kani_impl;")),
        "header must document the src/lib.rs placement line; got:\n{body}"
    );

    // Struct-based shape, shared with Anchor.
    assert!(
        compact(&body).contains(&compact("mod symbolic_accounts {"))
            && compact(&body).contains(&compact("pub fn build_bump() -> crate::Bump")),
        "must emit the symbolic accounts builder for crate::Bump; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("fn verify_bump_impl_ensures_0()"))
            && compact(&body).contains(&compact("accounts.handler(delta)")),
        "must emit the per-handler proof calling the real .handler(); got:\n{body}"
    );

    // The shared module comment must say "Quasar", not "Anchor".
    assert!(
        compact(&body).contains(&compact("host for this harness. Quasar")),
        "symbolic_accounts comment must be Quasar-flavored; got:\n{body}"
    );

    // Must NOT leak the word "Anchor" anywhere — the framework label
    // threads through every shared-emitter comment.
    assert!(
        !compact(&body).contains(&compact("Anchor")),
        "Quasar harness must not leak the Anchor framework name; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("struct AccountLayout"))
            && !compact(&body).contains(&compact("build_token_account")),
        "Quasar harness must not emit the Pinocchio stack scaffold; got:\n{body}"
    );

    let _ = std::fs::remove_file(&tmp);
}

/// Regression: a read-only field named only in `requires`/`ensures`
/// (never written, so absent from `modifies`/effects) must still be
/// snapshotted, and the `requires` assume must read the `pre_<field>`
/// snapshot — not the pure-model `s.<field>` accessor, which is unbound in
/// the harness. `threshold <= num_voters` is the archetype: `num_voters` is
/// read-only. Before the fix the harness referenced `s.num_voters` /
/// `post_num_voters` without declaring them, failing to compile.
#[test]
fn read_only_requires_ensures_fields_are_snapshotted() {
    let src = r#"spec WellFormed
state { threshold : U16, num_voters : U16 }
handler set_threshold (new_threshold : U16) {
  requires new_threshold <= state.num_voters else BadThreshold
  modifies [threshold]
  ensures state.threshold <= state.num_voters
  effect { threshold := new_threshold }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_readonly_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Quasar)
        .expect("kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // The read-only field is snapshotted on both sides of the call.
    assert!(
        compact(&body).contains(&compact("pre_num_voters"))
            && compact(&body).contains(&compact("post_num_voters")),
        "read-only field `num_voters` must be snapshotted pre and post; got:\n{body}"
    );
    // The `requires` assume reads the pre-snapshot, not the unbound `s.`
    // accessor.
    assert!(
        compact(&body).contains(&compact("kani::assume("))
            && compact(&body).contains(&compact("new_threshold <= pre_num_voters")),
        "requires assume must read `pre_num_voters`; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("s.num_voters")),
        "the pure-model `s.<field>` accessor must not leak into the impl harness; got:\n{body}"
    );
}

/// #162 phase 1: the brownfield-Anchor mode emits a state-struct harness
/// (symbolic state → agent-fill effect → assert ensures), NOT the greenfield
/// `Accounts` context + `accounts.handler(...)` shape that can't resolve
/// against a pre-existing Anchor program. Snapshots read from `state.<field>`
/// (incl. read-only requires/ensures fields), and only the struct
/// construction + effect application are `todo!()` agent-fill.
#[test]
fn brownfield_anchor_emits_state_struct_harness() {
    let src = r#"spec WellFormed
state { threshold : U16, num_voters : U16 }
handler set_threshold (new_threshold : U16) {
  requires new_threshold <= state.num_voters else BadThreshold
  modifies [threshold]
  ensures state.threshold <= state.num_voters
  effect { threshold := new_threshold }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_bf_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // Brownfield header, state-struct shape — NOT the greenfield Context shape.
    assert!(
        compact(&body).contains(&compact("BROWNFIELD Anchor")),
        "header names the brownfield mode; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("mod symbolic_accounts"))
            && !compact(&body).contains(&compact("let result = accounts.handler")),
        "must NOT emit the greenfield Accounts context / accounts.handler shape; got:\n{body}"
    );
    // The `requires` field is pre-snapshotted; the ensures reads its post fields
    // DIRECTLY off `state` by reference — no owned `post_` snapshot (the
    // drop-suppression shape, backlog R2).
    assert!(
        compact(&body).contains(&compact("let pre_num_voters = state.num_voters;"))
            && !compact(&body).contains(&compact("let post_num_voters ="))
            && compact(&body).contains(&compact("state.threshold <= state.num_voters")),
        "requires pre-snapshot; ensures reads post fields off `state`; got:\n{body}"
    );
    // requires assume reads the pre-snapshot (not the unbound `s.` accessor).
    assert!(
        compact(&body).contains(&compact("new_threshold <= pre_num_voters"))
            && !compact(&body).contains(&compact("s.num_voters")),
        "requires assume reads `pre_num_voters`; got:\n{body}"
    );
    // Exactly the two genuine agent-fill sites + the unwind hint. This spec is
    // numeric-only (no `Pubkey`), so the suggested bound is the low value —
    // not the 32-byte-memcmp 34 (see `unwind_bound_tracks_pubkey_presence`).
    assert!(
        compact(&body).contains(&compact("AGENT-FILL (1/2)"))
            && compact(&body).contains(&compact("AGENT-FILL (2/2)"))
            && compact(&body).contains(&compact("#[kani::unwind(4)]")),
        "two agent-fill sites + low unwind hint; got:\n{body}"
    );
}

/// #162 phase 2: when the spec declares its real on-chain struct via
/// `pragma state_struct = <Name>` and every State field is constructible
/// (scalar / `Pubkey` / `Option` / `Vec<record>` — the latter two landed with
/// #173/#174), the brownfield harness emits a fully-generated
/// `symbolic_<name>()` constructor and calls it — construction is NO LONGER
/// agent-fill. Only the effect + validity gate (AGENT-FILL 2/2) remains.
#[test]
fn brownfield_generates_symbolic_state_ctor_from_pragma() {
    let src = r#"spec SmartAccountProgram
pragma state_struct = Settings
type SmartAccountSigner = { key : Pubkey }
state {
  seed : U128,
  settings_authority : Pubkey,
  time_lock : U32,
  archival_authority : Option Pubkey,
  signers : Vec SmartAccountSigner,
  threshold : U16
}
handler set_time_lock (new_time_lock : U32) {
  modifies [time_lock]
  ensures state.time_lock == new_time_lock
  effect { time_lock := new_time_lock }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_ctor_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // The ctor targets the pragma-named struct, NOT the synthetic `crate::State`.
    assert!(
        compact(&body).contains(&compact("fn symbolic_settings() -> crate::Settings"))
            && !compact(&body).contains(&compact("crate::State")),
        "ctor builds the pragma-named `crate::Settings`; got:\n{body}"
    );
    // Every field constructed symbolically: scalars, Option (Some/None), Vec
    // (bounded loop), and the nested record.
    assert!(
        compact(&body).contains(&compact("seed: kani::any()")),
        "scalar field; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("archival_authority: if kani::any() { Some("))
            && compact(&body).contains(&compact(") } else { None }")),
        "Option<Pubkey> field symbolic Some/None; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("signers: vec![crate::SmartAccountSigner {"))
            && compact(&body).contains(&compact("key: anchor_lang::prelude::Pubkey::new_from_array(kani::any())"))
            && !compact(&body).contains(&compact("while ")),
        "Vec<record> is fixed-length vec![] with nested struct (no symbolic-length loop); got:\n{body}"
    );
    // The harness CALLS the ctor + assumes pre-state validity — construction is
    // no longer agent-fill; only the effect gate (2/2) is.
    assert!(
        compact(&body).contains(&compact("let mut state = core::mem::ManuallyDrop::new(symbolic_settings());"))
            && compact(&body).contains(&compact("kani::assume(state.invariant().is_ok());")),
        "harness calls the generated ctor (ManuallyDrop-wrapped, R2) + validity assume; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("AGENT-FILL (1/2)"))
            && compact(&body).contains(&compact("AGENT-FILL (2/2)")),
        "construction NOT agent-fill; only the effect gate is; got:\n{body}"
    );
}

/// `pragma kani_reject` emits a guard-enforcement (reject) proof per target
/// handler with a `requires`/`when` guard: it assumes the guard is VIOLATED and
/// asserts the real handler returns `Err` (bound to `!ok`) — the converse of
/// the ensures-preservation proof. Snapshots ONLY the guard's fields (not the
/// effect/modifies set). Absent the pragma, no reject harness is emitted.
#[test]
fn brownfield_kani_reject_emits_guard_enforcement_harness() {
    let src = r#"spec Guarded
pragma state_struct = Widget
pragma state_invariant = none
pragma kani_reject = on
state { size : U64, cap : U64 }
handler resize (n : U64) {
  requires n <= state.cap else TooBig
  modifies [size]
  ensures state.size == n
  effect { size := n }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_reject_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // The ensures-preservation harness is still emitted...
    assert!(
        compact(&body).contains(&compact("fn verify_resize_impl_ensures_0()")),
        "ensures harness still emitted; got:\n{body}"
    );
    // ...plus a reject harness that assumes the guard is VIOLATED (negated) and
    // asserts rejection.
    assert!(
        compact(&body).contains(&compact("fn verify_resize_rejects()")),
        "reject harness emitted; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("kani::assume(!(n <= pre_cap));")),
        "reject harness assumes the negated guard; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("assert!(!ok, \"resize must reject")),
        "reject harness asserts `!ok`; got:\n{body}"
    );
    // Snapshots the guard field (`cap`) — NOT the effect/modifies field (`size`).
    assert!(
        compact(&body).contains(&compact("let pre_cap = state.cap;")),
        "reject snapshots the guard field; got:\n{body}"
    );
}

/// `pragma kani_panic_free` emits a call-only proof per handler: construct
/// symbolic state, call the handler (agent-fill), no assertion — Kani's built-in
/// checks verify panic-freedom. Emitted even for a claim-free handler (no
/// ensures/effect), so the emitter must not bail early.
#[test]
fn brownfield_kani_panic_free_emits_call_only_proof() {
    let src = r#"spec PanicFree
pragma state_struct = Widget
pragma state_invariant = none
pragma kani_panic_free = on
state { size : U64, cap : U64 }
handler recompute (n : U64) {
  requires n <= state.cap else TooBig
  modifies [size]
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_pf_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("fn verify_recompute_panic_free()"))
            && compact(&body).contains(&compact(
                "let mut state = core::mem::ManuallyDrop::new(symbolic_widget());"
            )),
        "panic-free harness constructs symbolic state (ManuallyDrop-wrapped, R2); got:\n{body}"
    );
    // Panic-freedom is claimed UNDER the handler's preconditions — the `requires`
    // guard is assumed (not asserted).
    assert!(
        compact(&body).contains(&compact("kani::assume(n <= pre_cap);")),
        "panic-free harness assumes the `requires` guard; got:\n{body}"
    );
    // Call-only: no `assert!` and no ensures/reject scaffolding in this proof.
    assert!(
        !compact(&body).contains(&compact("assert!("))
            && !compact(&body).contains(&compact("_impl_ensures_")),
        "panic-free proof asserts nothing (Kani checks panics); got:\n{body}"
    );
}

/// A REQUIRES-ONLY handler (a guard, no `ensures`/`effect`) still gets a reject
/// proof under `pragma kani_reject` — guard enforcement is exactly where a
/// postcondition-free validator matters. The spec would otherwise emit nothing
/// (no ensures/effects to preserve), so the emitter must not bail early.
#[test]
fn brownfield_kani_reject_covers_requires_only_handler() {
    let src = r#"spec ReqOnly
pragma state_struct = Widget
pragma state_invariant = none
pragma kani_reject = on
state { size : U64, cap : U64 }
handler validate (n : U64) {
  requires n <= state.cap else TooBig
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_reqonly_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("fn verify_validate_rejects()"))
            && compact(&body).contains(&compact("kani::assume(!(n <= pre_cap));"))
            && compact(&body).contains(&compact("assert!(!ok, \"validate must reject")),
        "requires-only handler gets a reject proof; got:\n{body}"
    );
    // No ensures harness for a postcondition-free handler.
    assert!(
        !compact(&body).contains(&compact("_impl_ensures_")),
        "no ensures harness for a requires-only handler; got:\n{body}"
    );
}

/// Without `pragma kani_reject`, the reject harness is not emitted (default
/// output is unchanged).
#[test]
fn brownfield_without_kani_reject_pragma_omits_reject_harness() {
    let src = r#"spec Guarded
pragma state_struct = Widget
pragma state_invariant = none
state { size : U64, cap : U64 }
handler resize (n : U64) {
  requires n <= state.cap else TooBig
  modifies [size]
  ensures state.size == n
  effect { size := n }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_noreject_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        !compact(&body).contains(&compact("_rejects()"))
            && !compact(&body).contains(&compact("Guard-enforcement")),
        "no reject harness without the pragma; got:\n{body}"
    );
}

/// A brownfield harness whose `requires`/`ensures` use `is .Variant` + `len()`
/// over a non-`Copy` ADT status field renders shape-correctly and compiles:
///   - `is .StructVariant`  → `matches!(x, Enum::V { .. })` (resolved enum name,
///     struct pattern) — NOT the old `/* ty */::V(..)` stub;
///   - `is .UnitVariant`    → `matches!(x, Enum::V)` (no braces/parens);
///   - `len(coll)`          → `(coll.len() as u64)`;
///   - the non-`Copy` status snapshot `.clone()`s (a bare move would leave
///     `state` partially moved before the `&mut state` call);
///   - crate-level placement glob-imports `crate::*` so the bare enum name in
///     the `matches!` resolves.
///
/// (Regression for migrating hand-written proposal-consensus vote-registration
/// harnesses to the generated shape.)
#[test]
fn brownfield_isvariant_and_len_render_and_clone_nonstate_copy_field() {
    let src = r#"spec Consensus
pragma state_struct = Ballot
pragma state_invariant = none
type BallotStatus
  | Open of { at : I64 }
  | Carried of { at : I64 }
  | Tallying
type Error | NotOpen
state {
  status : BallotStatus,
  votes : Vec Pubkey,
  epoch : U8,
}
handler record (voter : Pubkey) (quorum : U64) {
  requires state.status is .Open else NotOpen
  modifies [status, votes]
  ensures (state.status is .Carried) implies (len(state.votes) >= quorum)
}
handler begin_tally (dummy : U64) {
  modifies [status]
  ensures (state.status is .Tallying) implies (dummy >= 1)
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_isvar_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // No lingering placeholder / wrong-shape stub anywhere.
    assert!(
        !compact(&body).contains(&compact("/* ty */"))
            && !compact(&body).contains(&compact("::Carried(..)")),
        "IsVariant must resolve the enum + shape, not emit the stub; got:\n{body}"
    );
    // Struct variants → resolved enum name + `{ .. }` pattern. Post reads go
    // through `state.<field>` directly (ManuallyDrop deref); pre reads its snapshot.
    assert!(
        compact(&body).contains(&compact("matches!(pre_status, BallotStatus::Open { .. })"))
            && compact(&body).contains(&compact(
                "matches!(state.status, BallotStatus::Carried { .. })"
            )),
        "struct-variant `is` → `Enum::V {{ .. }}`; got:\n{body}"
    );
    // Unit variant → bare `Enum::V`, no braces or parens.
    assert!(
        compact(&body).contains(&compact("matches!(state.status, BallotStatus::Tallying)"))
            && !compact(&body).contains(&compact("BallotStatus::Tallying {"))
            && !compact(&body).contains(&compact("BallotStatus::Tallying(")),
        "unit-variant `is` → `Enum::V` (no payload); got:\n{body}"
    );
    // `len(coll)` → `(coll.len() as u64)`, read off `state` for a post field.
    assert!(
        compact(&body).contains(&compact("(state.votes.len() as u64) >= quorum")),
        "len(coll) → `(coll.len() as u64)`; got:\n{body}"
    );
    // Non-Copy ADT status field: the PRE-snapshot `.clone()`s (a move would leave
    // `state` partially moved before the effect that follows). The POST side is NOT
    // snapshotted at all — the ensures reads `state.<field>` directly by reference
    // (drop-suppression, R2), so no owned `post_status`/`post_votes` local exists.
    assert!(
        compact(&body).contains(&compact("let pre_status = state.status.clone();"))
            && compact(&body).contains(&compact("let pre_votes = state.votes.clone();")),
        "non-Copy pre-snapshot must `.clone()`; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("let post_status ="))
            && !compact(&body).contains(&compact("let post_votes ="))
            && compact(&body).contains(&compact(
                "let mut state = core::mem::ManuallyDrop::new(symbolic_ballot());"
            )),
        "no owned post snapshot; state is ManuallyDrop-wrapped (R2); got:\n{body}"
    );
    // Crate-level placement glob-imports the crate root so the bare `BallotStatus`
    // name in the `matches!` resolves; the ctor still qualifies with `crate::`.
    assert!(
        compact(&body).contains(&compact("use crate::*;"))
            && compact(&body).contains(&compact("_ => crate::BallotStatus::Tallying")),
        "crate-level harness imports `crate::*`; ctor unit arm has no braces; got:\n{body}"
    );
}

/// `pragma kani_vec_empty = <field>` builds that `Vec` field as `vec![]` — no
/// element construction — so a heavy/irrelevant `Vec<T>` field costs nothing and
/// its element type `T` need not even be declared in the spec (only the field
/// type name). Also lets a `match` bind a payload behind a `&` (via `.clone()`).
#[test]
fn brownfield_kani_vec_empty_skips_element_construction() {
    let src = r#"spec VecEmpty
pragma state_struct = Holder
pragma state_invariant = none
pragma kani_vec_empty = big
state { big : Vec UndeclaredBigType, n : U8 }
handler t (m : U8) { modifies [n] ensures state.n == m effect { n := m } }"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_vecempty_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // The field is `vec![]` — `UndeclaredBigType` is never constructed (would
    // otherwise bail the whole ctor since it isn't a declared record/sum type).
    assert!(
        compact(&body).contains(&compact("big: vec![],"))
            && compact(&body).contains(&compact("fn symbolic_holder()")),
        "kani_vec_empty field is `vec![]`, ctor still emitted; got:\n{body}"
    );
}

/// `match` on an `Option` field binds `Some`'s payload and renders the builtin
/// prelude variants shape-correctly (`Option::Some(h)` tuple / `Option::None`
/// unit) — composing with `exists x in coll` + field access to navigate a nested
/// `Option<Record>` with a `Vec` field (the E-A predicate shape).
#[test]
fn brownfield_option_match_composes_with_exists_in() {
    let src = r#"spec OptMatch
pragma state_struct = Pol
pragma state_invariant = none
type Hook = { keys : Vec Pubkey }
state { post_hook : Option Hook, n : U8 }
handler check (auth : Pubkey) {
  modifies [n]
  ensures (match state.post_hook with | Some h => (exists k in h.keys, k == auth) | None => false)
  effect { n := 0 }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_optmatch_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact(
            "Option::Some(h) => (h.keys.iter().any(|k| k == auth))"
        )) && compact(&body).contains(&compact("Option::None => false")),
        "Option match binds Some's payload + composes with exists-in + field access; got:\n{body}"
    );
}

/// `pragma kani_option_none = <field>` builds that `Option<_>` field as `None`
/// — no `Some` payload construction — so a dead symbolic sub-state the property
/// never reads costs nothing. Companion to `kani_vec_empty` for pruning
/// nested-container construction that would otherwise blow up CBMC.
#[test]
fn brownfield_kani_option_none_prunes_payload() {
    let src = r#"spec OptNone
pragma state_struct = Pol
pragma state_invariant = none
pragma kani_option_none = pre_hook
type Hook = { keys : Vec Pubkey }
state { pre_hook : Option Hook, post_hook : Option Hook, n : U8 }
handler check (auth : Pubkey) {
  modifies [n]
  ensures (match state.post_hook with | Some h => (exists k in h.keys, k == auth) | None => false)
  effect { n := 0 }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_optnone_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // `pre_hook` is `None` (no `Some(Hook { .. })` payload construction); the
    // read `post_hook` still gets a full symbolic `if kani::any() { Some(..) }`.
    assert!(
        compact(&body).contains(&compact("pre_hook: None,"))
            && compact(&body).contains(&compact("post_hook: if kani::any()")),
        "kani_option_none field is `None`; the read Option stays symbolic; got:\n{body}"
    );
}

/// The brownfield ensures harness snapshots a field on the side that actually
/// reads it: a field read *only* via `post.<x>` gets a `post_<x>` clone and NO
/// dead `pre_<x>` clone (which, for a non-`Copy` `Vec`, would deep-copy + drop
/// the whole container and inflate CBMC's VCC count). Effect-participating
/// fields (`modifies`) still snapshot on both sides for the old/new comparison.
#[test]
fn brownfield_snapshot_split_drops_dead_pre_clone() {
    let src = r#"spec SnapSplit
pragma state_struct = Pol
pragma state_invariant = none
type Hook = { keys : Vec Pubkey }
state { post_hook : Option Hook, n : U8 }
handler check (auth : Pubkey) {
  modifies [n]
  ensures (match state.post_hook with | Some h => (exists k in h.keys, k == auth) | None => false)
  effect { n := 0 }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_snapsplit_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // `post_hook` is read only via `post.` → no dead `pre_post_hook` clone (the
    // pre/post split still holds), and — under drop-suppression (R2) — no owned
    // `post_post_hook` snapshot either: the ensures reads `state.post_hook`
    // directly by reference off the `ManuallyDrop`-wrapped state.
    assert!(
        !compact(&body).contains(&compact("let post_post_hook"))
            && !compact(&body).contains(&compact("let pre_post_hook"))
            && compact(&body).contains(&compact("match &(state.post_hook)")),
        "post-only field read off `state` by ref; no owned snapshot, no dead pre_ clone; got:\n{body}"
    );
}

/// Drop-suppression (backlog R2): the brownfield ensures harness `ManuallyDrop`-
/// wraps the symbolic state and reads `post.<field>` DIRECTLY off `state` by
/// reference — no owned `post_<field>` snapshot is moved out, so the symbolic
/// nested container is never dropped. That teardown machinery
/// (`drop_in_place::<[T]>` / `RawVec::deallocate`), not the property, is what
/// OOM'd CBMC's propositional reduction on deeply-nested state; suppressing it
/// took a real E-A harness from 20,322 VCCs (OOM) to 2,395 (closes in seconds).
#[test]
fn brownfield_drop_suppression_manually_drops_and_reads_by_ref() {
    with_big_stack(|| {
        let src = r#"spec PostMove
pragma state_struct = Pol
pragma state_invariant = none
type Kind | Keys of Vec Pubkey | Other
type Con = { kind : Kind }
type Hook = { cons : Vec Con }
state { post_hook : Option Hook, n : U8 }
handler check (auth : Pubkey) {
  modifies [n]
  ensures (match state.post_hook with | Some h => not (exists c in h.cons, (match c.kind with | Keys pks => contains(pks, auth) | _ => false)) | None => true)
  effect { n := 0 }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp =
            std::env::temp_dir().join(format!("kani_impl_postmove_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec_with_mode(
            &spec,
            &tmp,
            /*explicit_flag=*/ true,
            Target::Anchor,
            KaniImplMode::Brownfield,
        )
        .expect("brownfield kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        // State is `ManuallyDrop`-wrapped; the ensures reads it by reference with no
        // owned `post_post_hook` snapshot.
        assert!(
            compact(&body).contains(&compact(
                "let mut state = core::mem::ManuallyDrop::new(symbolic_pol());"
            )) && !compact(&body).contains(&compact("let post_post_hook")),
            "ManuallyDrop state, no owned post snapshot; got:\n{body}"
        );
        // Both the outer snapshot match AND the inner `.iter()` enum match render by
        // reference — NO defensive `.clone()` scrutinee survives, so the enum's `Vec`
        // payloads never regenerate the `drop_in_place` teardown (the difference
        // between a harness that times out and one that closes in seconds).
        assert!(
            compact(&body).contains(&compact("match &(state.post_hook)"))
                && compact(&body).contains(&compact("match &(c.kind)"))
                && !compact(&body).contains(&compact(").clone() {")),
            "outer + inner matches by-ref; no clone-form scrutinee in the ensures; got:\n{body}"
        );
    });
}

/// `exists|forall x in <coll>, pred(x)` — a bounded quantifier over a collection
/// value — lowers to `coll.iter().any|all(|x| pred)`, binding each element (with
/// field access). The "some/every element of a collection satisfies P" primitive.
#[test]
fn brownfield_quant_in_collection_lowering() {
    let src = r#"spec QuantIn
pragma state_struct = Roster
pragma state_invariant = none
type Signer = { key : Pubkey, mask : U8 }
state { signers : Vec Signer, cap : U8 }
handler check (lo : U8) {
  modifies [cap]
  ensures forall s in state.signers, s.mask >= lo
  effect { cap := lo }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_quantin_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact(".iter().all(|s| s.mask >= lo)")),
        "forall-in → `.iter().all(|x| pred)` with element field access; got:\n{body}"
    );
}

/// `pragma kani_abstract_div = on` emits the `checked_div_abstract` support fn
/// once + a `#[kani::stub(i64::checked_div, checked_div_abstract)]` per proof —
/// the #182 arithmetic tier that removes the symbolic-divisor circuit that
/// stalls both SAT and SMT backends.
#[test]
fn brownfield_kani_abstract_div_emits_stub() {
    let src = r#"spec DivAbs
pragma state_struct = Widget
pragma state_invariant = none
pragma kani_abstract_div = on
state { size : U64, cap : U64 }
handler resize (n : U64) {
  modifies [size]
  ensures state.size == n
  effect { size := n }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_divabs_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("fn checked_div_abstract(a: i64, b: i64) -> Option<i64>"))
            && compact(&body).contains(&compact("#[kani::stub(i64::checked_div, checked_div_abstract)]"))
            // #182 Shape-1: the exact-contract logic lives in the proven crate.
            && compact(&body).contains(&compact("qedgen_kani_prelude::checked_div_i64(a, b)")),
        "kani_abstract_div emits the stub fn + attr, delegating to qedgen_kani_prelude; got:\n{body}"
    );
}

/// `pragma kani_solver = <solver>` bakes `#[kani::solver(<solver>)]` into every
/// generated proof (right after `#[kani::proof]`), so a harness that needs an
/// SMT solver (e.g. z3 for symbolic `checked_div`) is reproducible without a
/// `--solver` flag.
#[test]
fn brownfield_kani_solver_pragma_bakes_solver_attr() {
    let src = r#"spec SolverTest
pragma state_struct = Widget
pragma state_invariant = none
pragma kani_solver = z3
state { size : U64, cap : U64 }
handler resize (n : U64) {
  modifies [size]
  ensures state.size == n
  effect { size := n }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_solver_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("#[kani::proof]\n#[kani::solver(z3)]")),
        "kani_solver pragma bakes `#[kani::solver(z3)]` after `#[kani::proof]`; got:\n{body}"
    );
}

/// A `match` in `requires` binds a TUPLE variant's payload and renders a
/// shape-correct, enum-resolved pattern with a `_` catch-all — the vehicle for
/// "if period is Custom(s) then s > 0" preconditions (variant payload binding).
#[test]
fn brownfield_match_payload_binding_in_requires() {
    let src = r#"spec PayloadMatch
pragma state_struct = Timer
pragma state_invariant = none
type PeriodV2 | OneTime | Daily | Custom of I64
state { period : PeriodV2, n : U64 }
handler tick (m : U64) {
  requires (match state.period with | Custom s => s > 0 | _ => true) else BadPeriod
  modifies [n]
  ensures state.n == m
  effect { n := m }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_pmatch_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // Enum resolved, tuple payload bound to `s`, wildcard catch-all — no stub.
    assert!(
        compact(&body).contains(&compact("PeriodV2::Custom(s) => s > 0"))
            && compact(&body).contains(&compact("_ => true"))
            && !compact(&body).contains(&compact("/* ty */")),
        "match binds the tuple payload with a resolved enum + wildcard; got:\n{body}"
    );
}

/// `is .Variant` renders the shape-correct `matches!` pattern for all three
/// variant shapes (G13b IsVariant): TUPLE (`Custom of I64` → `Enum::V(..)`),
/// UNIT (`Enum::V`), and STRUCT (`Enum::V { .. }`).
#[test]
fn brownfield_isvariant_tuple_unit_struct_patterns() {
    let src = r#"spec ShapeTest
pragma state_struct = Timer
pragma state_invariant = none
type PeriodV2 | OneTime | Custom of I64
type Status | Active of { at : I64 } | Approved of { at : I64 }
state { period : PeriodV2, status : Status, n : U64 }
handler tick (m : U64) {
  modifies [n]
  ensures (state.period is .Custom) or (state.period is .OneTime) or (state.status is .Approved)
  effect { n := m }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_shapes_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("PeriodV2::Custom(..)")),
        "tuple variant → `Enum::V(..)`; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("PeriodV2::OneTime"))
            && !compact(&body).contains(&compact("PeriodV2::OneTime("))
            && !compact(&body).contains(&compact("PeriodV2::OneTime {")),
        "unit variant → bare `Enum::V`; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("Status::Approved { .. }")),
        "struct variant → `Enum::V {{ .. }}`; got:\n{body}"
    );
}

/// #183 / G17b: an in-module brownfield harness (`pragma state_module`) whose
/// mirrored State references types from a SECOND private module can't name them
/// via `use super::*` alone. `pragma harness_use = <path>` (repeatable) injects
/// the missing `use` lines verbatim — a `::*` glob or a single type path, in
/// source order, under one `#[allow(unused_imports)]`.
#[test]
fn brownfield_harness_use_pragma_injects_extra_imports() {
    let src = r#"spec HarnessUse
pragma state_struct = Widget
pragma state_module = state::widgets::widget
pragma state_invariant = none
pragma harness_use = crate::state::widgets::parts::*
pragma harness_use = crate::core::traits::WidgetTrait
state { size : U64, kind : Kind }
type Kind | Small | Large
handler resize (n : U64) {
  modifies [size]
  ensures state.size == n
  effect { size := n }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_hu_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // In-module placement header, then the two requested `use` paths
    // verbatim (glob + single type). Relative ordering is owned by rustfmt
    // (the write seam formats output, and rustfmt sorts consecutive `use`
    // groups), so only presence is asserted.
    assert!(
        compact(&body).contains(&compact("use super::*;")),
        "state_module → in-module placement header; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("use crate::state::widgets::parts::*;"))
            && compact(&body).contains(&compact("use crate::core::traits::WidgetTrait;")),
        "harness_use paths emitted verbatim (glob + single type); got:\n{body}"
    );
}

/// F2 (#167): the impl-harness unwind bound is computed, not fixed. A harness
/// that snapshots or takes a `Pubkey` (→ `[u8; 32]`, a 32-byte `memcmp`)
/// suggests `#[kani::unwind(34)]`; a numeric-only harness suggests a low bound
/// (< 34) so it closes faster. Covers greenfield (Anchor) both ways plus
/// brownfield Pubkey.
#[test]
fn unwind_bound_tracks_pubkey_presence() {
    // (a) Pubkey STATE field compared in `ensures` → high bound.
    let pk_state = r#"spec Registry
state { authority : Pubkey, count : U64 }
handler rotate (new_authority : Pubkey) {
  modifies [authority]
  ensures state.authority == new_authority
  effect { authority := new_authority }
}"#;
    let spec = parse_str(pk_state).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_unwind_pk_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        compact(&body).contains(&compact("#[kani::unwind(34)]"))
            && !compact(&body).contains(&compact("#[kani::unwind(4)]")),
        "Pubkey-comparing harness must suggest unwind 34; got:\n{body}"
    );

    // (b) Numeric-only handler → low bound (< 34).
    let numeric = r#"spec Counter
state { count : U64, total : U64 }
handler bump (delta : U64) {
  requires delta > 0 else BadDelta
  modifies [count]
  ensures state.count == old(state.count) + delta
  effect { count += delta }
}"#;
    let spec = parse_str(numeric).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_unwind_num_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        compact(&body).contains(&compact("#[kani::unwind(4)]"))
            && !compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "numeric-only harness must suggest a low unwind bound; got:\n{body}"
    );

    // (c) Pubkey handler PARAM alone (no Pubkey state field) still lifts the
    //     brownfield bound to 34.
    let pk_param = r#"spec Guarded
state { count : U64 }
handler admin_bump (caller : Pubkey) (delta : U64) {
  requires delta > 0 else BadDelta
  modifies [count]
  ensures state.count == old(state.count) + delta
  effect { count += delta }
}"#;
    let spec = parse_str(pk_param).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_unwind_bfpk_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    // With the #182 Tier-1 Pubkey abstraction ON (default), the brownfield
    // harness stubs Pubkey `==` to a wide-integer compare and drops the
    // memcmp-driven bound to a small value (the 34 is only needed with the
    // abstraction OFF — see the opt-out case below).
    assert!(
        compact(&body).contains(&compact("fn pk_eq_abstract"))
            // #182 Shape-1: the wide-compare logic lives in the proven crate;
            // the harness only bridges its own Pubkey to the byte-level API.
            && compact(&body).contains(&compact("qedgen_kani_prelude::wide_eq_32(a.to_bytes(), b.to_bytes())"))
            && compact(&body).contains(&compact("qedgen_kani_prelude::wide_cmp_32(a.to_bytes(), b.to_bytes())"))
            && compact(&body).contains(&compact("kani::stub(<anchor_lang::prelude::Pubkey"))
            && !compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "brownfield Pubkey harness abstracts `==`/`cmp` via qedgen_kani_prelude + drops the bound; got:\n{body}"
    );

    // (d) A Pubkey STATE field that is NEVER referenced in a guard/ensures (only
    //     numeric fields are) must STILL lift the bound: impl harnesses call
    //     real code over the whole struct, whose owner/has_one/dedup checks do a
    //     32-byte memcmp. Regression for the settings well-formedness shape,
    //     where `authority: Pubkey` drives `auth`/`has_one` but appears in no
    //     guard/ensures expression — an intersection-with-snapshot heuristic
    //     would wrongly pick 4 and re-introduce the unwinding-assertion failure.
    let pk_unref = r#"spec Settingsish
state { authority : Pubkey, threshold : U16, voters : U16 }
handler set_threshold (new_threshold : U16) {
  requires new_threshold <= state.voters else Bad
  modifies [threshold]
  ensures state.threshold <= state.voters
  effect { threshold := new_threshold }
}"#;
    let spec = parse_str(pk_unref).expect("parse");
    let tmp =
        std::env::temp_dir().join(format!("kani_impl_unwind_unref_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    // Abstraction ON (default): stubbed + small bound, even for the
    // unreferenced-Pubkey settings-well-formedness shape.
    assert!(
        compact(&body).contains(&compact("fn pk_eq_abstract"))
            && !compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "unreferenced Pubkey field: abstracted + small bound; got:\n{body}"
    );

    // (e) Opt-out `pragma kani_abstract_pubkey = off` → no stub, memcmp bound 34.
    let pk_optout = r#"spec SettingsishOff
pragma kani_abstract_pubkey = off
state { authority : Pubkey, threshold : U16, voters : U16 }
handler set_threshold (new_threshold : U16) {
  requires new_threshold <= state.voters else Bad
  modifies [threshold]
  ensures state.threshold <= state.voters
  effect { threshold := new_threshold }
}"#;
    let spec = parse_str(pk_optout).expect("parse");
    let tmp =
        std::env::temp_dir().join(format!("kani_impl_unwind_optout_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        !compact(&body).contains(&compact("fn pk_eq_abstract"))
            && compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "opt-out: no Pubkey stub, memcmp bound 34; got:\n{body}"
    );
}

/// Slice 8 M3: Pinocchio emits a stack-allocated `AccountInfo`
/// harness. Validates the deterministic scaffold + per-handler
/// proof shape that the M2 reference
/// (crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/src/kani_impl.rs)
/// proved catches real overflow bugs.
#[test]
fn pinocchio_target_emits_stack_harness() {
    // SPL-transfer-shaped handler: two explicit token accounts
    // (source, destination), a readonly mint, a signer authority.
    let src = r#"spec PtokenTransfer
state { dummy : U64 }
handler transfer (amount : U64) {
  accounts {
    source : writable, token
    mint : readonly
    destination : writable, token
    authority : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_pinocchio_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    assert!(tmp.is_file(), "Pinocchio target must write a harness file");
    let body = std::fs::read_to_string(&tmp).unwrap();

    // Deterministic scaffold present.
    assert!(
        compact(&body).contains(&compact("struct AccountLayout")),
        "must emit the Account layout mirror; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact(
            "assert!(core::mem::size_of::<AccountLayout>() == 88)"
        )),
        "must emit the layout size assertion; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("fn build_token_account"))
            && compact(&body).contains(&compact("fn build_minimal_account"))
            && compact(&body).contains(&compact("fn account_info_from_stack")),
        "must emit the build + transmute helpers; got:\n{body}"
    );

    // Per-handler proof.
    assert!(
        compact(&body).contains(&compact("#[kani::proof]"))
            && compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "must emit the proof attribute + memcmp unwind bound; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("fn verify_transfer_impl()")),
        "must emit the per-handler proof fn; got:\n{body}"
    );

    // Account classification: explicit token accounts -> token account;
    // signer/readonly → minimal.
    assert!(
        compact(&body).contains(&compact("let mut source = build_token_account("))
            && compact(&body).contains(&compact("let mut destination = build_token_account(")),
        "explicit token accounts must build as token accounts; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("let mut mint = build_minimal_account("))
            && compact(&body).contains(&compact("let mut authority = build_minimal_account(")),
        "readonly + signer accounts must build as minimal accounts; got:\n{body}"
    );

    // Param packing + real dispatcher call.
    assert!(
        compact(&body).contains(&compact("let amount: u64 = kani::any();"))
            && compact(&body).contains(&compact("let instruction_tag: u8 = crate::TRANSFER;"))
            && compact(&body).contains(&compact("instruction_data.push(instruction_tag);"))
            && compact(&body).contains(&compact(
                "instruction_data.extend_from_slice(&amount.to_le_bytes());"
            )),
        "U64 param must be symbolic + tag/LE-packed; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact(
            "crate::process_instruction(&program_id, accounts_slice, &instruction_data)"
        )),
        "must call the real process_instruction dispatcher; got:\n{body}"
    );

    // Must NOT leak the Anchor shape.
    assert!(
        !compact(&body).contains(&compact("Context<"))
            && !compact(&body).contains(&compact("symbolic_accounts")),
        "Pinocchio harness must not leak the Anchor Context shape; got:\n{body}"
    );

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn pinocchio_dispatcher_packs_numeric_params_in_spec_order() {
    let src = r#"spec Pool
state { lane_count : U64 }
handler batch_16
  (amount_0 : U64) (from_lane_id_0 : U64) (to_lane_id_0 : U64)
  (amount_1 : U64) (from_lane_id_1 : U64) (to_lane_id_1 : U64)
  (amount_2 : U64) (from_lane_id_2 : U64) (to_lane_id_2 : U64)
  (amount_3 : U64) (from_lane_id_3 : U64) (to_lane_id_3 : U64)
  (amount_4 : U64) (from_lane_id_4 : U64) (to_lane_id_4 : U64)
  (amount_5 : U64) (from_lane_id_5 : U64) (to_lane_id_5 : U64)
  (amount_6 : U64) (from_lane_id_6 : U64) (to_lane_id_6 : U64)
  (amount_7 : U64) (from_lane_id_7 : U64) (to_lane_id_7 : U64)
  (amount_8 : U64) (from_lane_id_8 : U64) (to_lane_id_8 : U64)
  (amount_9 : U64) (from_lane_id_9 : U64) (to_lane_id_9 : U64)
  (amount_10 : U64) (from_lane_id_10 : U64) (to_lane_id_10 : U64)
  (amount_11 : U64) (from_lane_id_11 : U64) (to_lane_id_11 : U64)
  (amount_12 : U64) (from_lane_id_12 : U64) (to_lane_id_12 : U64)
  (amount_13 : U64) (from_lane_id_13 : U64) (to_lane_id_13 : U64)
  (amount_14 : U64) (from_lane_id_14 : U64) (to_lane_id_14 : U64)
  (amount_15 : U64) (from_lane_id_15 : U64) (to_lane_id_15 : U64) {
  accounts {
    config : readonly
    inventory_rebalancer : signer
    token_program : readonly
    mint : readonly
    source_authority_0 : readonly
    source_inventory_0 : writable
    destination_inventory_0 : writable
  }
  ensures state.lane_count == old(state.lane_count)
  effect { lane_count := lane_count }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_batch_pack_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        compact(&body).contains(&compact("let instruction_tag: u8 = crate::BATCH;"))
            && compact(&body).contains(&compact(
                "instruction_data.extend_from_slice(&amount_0.to_le_bytes());"
            ))
            && compact(&body).contains(&compact(
                "instruction_data.extend_from_slice(&from_lane_id_15.to_le_bytes());"
            ))
            && compact(&body).contains(&compact(
                "instruction_data.extend_from_slice(&to_lane_id_15.to_le_bytes());"
            ))
            && compact(&body).contains(&compact(
                "instruction_data.extend_from_slice(&amount_15.to_le_bytes());"
            )),
        "generic Pinocchio packing must use the base tag and declared numeric params; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("instruction_data.push(16u8);"))
            && !compact(&body).contains(&compact("from_lane_id_15 as u8"))
            && !compact(&body).contains(&compact("to_lane_id_15 as u8"))
            && !compact(&body).contains(&compact("crate::BATCH_16")),
        "runtime-specific arity bytes and narrowing casts require an ABI profile; got:\n{body}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn pinocchio_impl_packs_abi_repeated_records_from_indexed_params() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
limit MAX_ITEMS 4
instruction BATCH 4

record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
field AMOUNT u64
end

record BATCH_ARGS
field ITEM_COUNT u8
repeat ITEM transfer MAX_ITEMS ITEM_COUNT
end

instruction_record BATCH BATCH_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec Pool
state { lane_count : U64 }
handler batch_2
  (amount_0 : U64) (from_lane_id_0 : U64) (to_lane_id_0 : U64)
  (amount_1 : U64) (from_lane_id_1 : U64) (to_lane_id_1 : U64) {
  accounts {
    config : readonly
    source_0 : writable
    destination_0 : writable
  }
  ensures state.lane_count == old(state.lane_count)
  effect { lane_count := lane_count }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
        compact(&body).contains(&compact("let instruction_tag: u8 = 4u8;"))
            && compact(&body).contains(&compact("instruction_data[1] = 2u8;"))
            && compact(&body).contains(&compact(
                "instruction_data[2] = (from_lane_id_0 as u8) as u8;"
            ))
            && compact(&body).contains(&compact(
                "instruction_data[3] = (to_lane_id_0 as u8) as u8;"
            ))
            && compact(&body).contains(&compact(
                "let generated_instruction_data_4_bytes = (amount_0 as u64).to_le_bytes();"
            ))
            && compact(&body).contains(&compact(
                "instruction_data[12] = (from_lane_id_1 as u8) as u8;"
            ))
            && compact(&body).contains(&compact(
                "instruction_data[13] = (to_lane_id_1 as u8) as u8;"
            ))
            && compact(&body).contains(&compact(
                "let generated_instruction_data_14_bytes = (amount_1 as u64).to_le_bytes();"
            )),
        "ABI repeat profile must pack count and indexed item fields in ABI order; got:\n{body}"
    );
    assert!(
            !compact(&body).contains(&compact("source profile references param `item_count` absent")),
            "repeat count should be derived from indexed params, not treated as a missing param; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_emits_verified_stubs_for_contracted_source_helpers() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(
        program_root.join("src/lib.rs"),
        "mod processor;\nmod validation;\n",
    )
    .unwrap();
    std::fs::write(
        program_root.join("src/validation.rs"),
        r#"
#[cfg_attr(kani, kani::requires(amount > 0))]
#[cfg_attr(kani, kani::ensures(|result| result.is_ok()))]
pub fn check_amount(amount: u64) -> Result<(), ()> {
    if amount == 0 { Err(()) } else { Ok(()) }
}
"#,
    )
    .unwrap();
    std::fs::write(
        program_root.join("src/processor.rs"),
        r#"
use crate::validation::check_amount;

pub fn process_transfer(_accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    check_amount(amount)?;
    Ok(())
}
"#,
    )
    .unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
instruction TRANSFER 1

record TRANSFER_ARGS
field AMOUNT u64
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec Pool
state { dummy : U64 }
handler transfer (amount : U64) {
  accounts { payer : signer }
  requires amount > 0 else InvalidAmount
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("#[kani::stub_verified(crate::validation::check_amount)]"))
                && compact(&body).contains(&compact("fn verify_transfer_impl()"))
                && compact(&body).contains(&compact("crate::process_instruction(&program_id")),
            "contracted source helper calls should emit verified stubs on the real-dispatcher harness; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_packs_abi_repeated_pubkey_fields_from_indexed_params() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
limit MAX_ITEMS 4
instruction BATCH 4

record TRANSFER
field MINT pubkey
field AMOUNT u64
end

record BATCH_ARGS
field ITEM_COUNT u8
repeat ITEM transfer MAX_ITEMS ITEM_COUNT
end

instruction_record BATCH BATCH_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec PubkeyBatch
state { total : U64 }
handler batch_2
  (mint_0 : Pubkey) (amount_0 : U64)
  (mint_1 : Pubkey) (amount_1 : U64) {
  accounts { config : readonly }
  ensures state.total == old(state.total)
  effect { total := total }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
        compact(&body).contains(&compact(
            "let mint_0: [u8; 32] = kani::any(); // spec type: Pubkey"
        )) && compact(&body).contains(&compact(
            "let mint_1: [u8; 32] = kani::any(); // spec type: Pubkey"
        )),
        "indexed Pubkey repeat fields must be declared as symbolic 32-byte arrays; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("instruction_data[1] = 2u8;"))
            && compact(&body).contains(&compact(
                "write_fixed_32(&mut instruction_data, 2, mint_0);"
            ))
            && compact(&body).contains(&compact(
                "let generated_instruction_data_34_bytes = (amount_0 as u64).to_le_bytes();"
            ))
            && compact(&body).contains(&compact(
                "write_fixed_32(&mut instruction_data, 42, mint_1);"
            ))
            && compact(&body).contains(&compact(
                "let generated_instruction_data_74_bytes = (amount_1 as u64).to_le_bytes();"
            )),
        "ABI repeat profile must pack indexed Pubkey fields in ABI order; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("TODO: pack repeat field `mint`")),
        "Pubkey repeat fields should no longer be dropped from the ABI profile; got:\n{body}"
    );
}

#[test]
fn pinocchio_impl_emits_token_transfer_balance_assertions() {
    let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : writable
    destination : writable
    authority : signer
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!(
        "kani_impl_token_assertions_{}.rs",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        compact(&body).contains(&compact(
            "let pre_transfer_0_from = read_token_amount(&source);"
        )) && compact(&body).contains(&compact(
            "let pre_transfer_0_to = read_token_amount(&destination);"
        )) && compact(&body).contains(&compact(
            "kani::assume(pre_transfer_0_from >= (amount as u64));"
        )) && compact(&body).contains(&compact(
            "kani::assume(pre_transfer_0_to <= u64::MAX - (amount as u64));"
        )),
        "must snapshot and constrain Token.transfer balances; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("if _result.is_ok() {"))
            && compact(&body).contains(&compact(
                "assert_eq!(read_token_amount(&source), pre_transfer_0_from - (amount as u64));"
            ))
            && compact(&body).contains(&compact(
                "assert_eq!(read_token_amount(&destination), pre_transfer_0_to + (amount as u64));"
            )),
        "must assert Token.transfer balance deltas on success; got:\n{body}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn pinocchio_impl_does_not_classify_all_writable_accounts_as_tokens() {
    let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    config : writable
    source : writable, token
    destination : writable, token
    authority : signer
    token_program : program, type token
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_token_roles_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();

    assert!(
            compact(&body).contains(&compact("let mut config = build_minimal_account("))
                && compact(&body).contains(&compact("let mut source = build_token_account("))
                && compact(&body).contains(&compact("let mut destination = build_token_account("))
                && compact(&body).contains(&compact("let mut token_program = build_minimal_account("))
                && !compact(&body).contains(&compact("let config_amount: u64 = kani::any();")),
            "only explicit token accounts or Token.transfer resources should use token layout; got:\n{body}"
        );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn pinocchio_impl_uses_abi_account_roles_for_token_projection() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
instruction MOVE_TOKENS 8
account MOVE_TOKENS SOURCE 0 writable type token
account MOVE_TOKENS DESTINATION 1 writable type token
account MOVE_TOKENS MINT 2 type mint
account MOVE_TOKENS TOKEN_PROGRAM 3 program type token

record MOVE_TOKENS_ARGS
field AMOUNT u64
end

instruction_record MOVE_TOKENS MOVE_TOKENS_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : readonly
    destination : readonly
    mint : readonly
    token_program : program
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let mut source = build_token_account([1u8; 32], true, false"))
                && compact(&body)
                    .contains(&compact("let mut destination = build_token_account([2u8; 32], true, false"))
                && compact(&body).contains(&compact("let mut mint = build_mint_account([3u8; 32], false, false, 6u8);"))
                && compact(&body).contains(&compact("let mut token_program = build_minimal_account(SPL_TOKEN_PROGRAM_ID, false, false)"))
                && !compact(&body).contains(&compact("let token_program_amount: u64 = kani::any();")),
            "ABI account roles should project token accounts and mints without treating token_program as token data; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_projects_source_inferred_token_account_mint_and_owner() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::write(
        program_root.join("src/lib.rs"),
        r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        8 => process_move_tokens(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_move_tokens(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let source = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let authority = next_account_info(account_info_iter)?;
    require_token_account(source, mint.key(), authority.key())?;
    let decimals = read_mint_decimals(mint)?;
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec TokenProjection
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : writable
    mint : readonly
    authority : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let source_amount: u64 = kani::any();"))
                && compact(&body).contains(&compact("let mut source = build_token_account([1u8; 32], true, false, [2u8; 32], authority_key, (source_amount as u64));"))
                && compact(&body).contains(&compact("let mut mint = build_mint_account([2u8; 32], false, false, 6u8);")),
            "source-inferred token account bindings should project mint and owner bytes; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_projects_repeated_token_binding_from_key_alias() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(program_root.join("schema")).unwrap();
    std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn derive_authority(program_id: &pinocchio::pubkey::Pubkey, lane_id: u8) -> ([u8; 32], u8) {
    pinocchio::pubkey::try_find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id).unwrap()
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        9 => process_move_tokens(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_move_tokens(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let source = next_account_info(account_info_iter)?;
    let destination = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let source_authority = next_account_info(account_info_iter)?;
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(8..9)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let source_authority_key = derive_authority(program_id, 0).0;
    let destination_authority_key = derive_authority(program_id, lane_id).0;
    require_key(source_authority, &source_authority_key)?;
    require_token_account(source, mint.key(), &source_authority_key)?;
    require_token_account(destination, mint.key(), &destination_authority_key)?;
    let decimals = read_mint_decimals(mint)?;
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
        )
        .unwrap();
    std::fs::write(
        program_root.join("schema/program.schema"),
        "seed AUTHORITY_SEED authority\n",
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec TokenProjection
state { dummy : U64 }
handler move_tokens (amount : U64) (lane_id : U64) {
  accounts {
    source_0 : writable
    destination_0 : writable
    mint : readonly
    source_authority_0 : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let source_authority_0_key = crate::derive_authority(&program_id, lane_id as u8).0;"))
                && compact(&body).contains(&compact("let source_0_amount: u64 = kani::any();"))
                && compact(&body).contains(&compact("let mut source_0 = build_token_account([1u8; 32], true, false, [3u8; 32], source_authority_0_key, (source_0_amount as u64));")),
            "repeated token account should inherit source loop binding and owner key alias; got:\n{body}"
        );
    assert!(
            compact(&body).contains(&compact("let destination_0_owner_key = crate::derive_authority(&program_id, lane_id as u8).0;"))
                && compact(&body).contains(&compact("let destination_0_amount: u64 = kani::any();"))
                && compact(&body).contains(&compact("let mut destination_0 = build_token_account([2u8; 32], true, false, [3u8; 32], destination_0_owner_key, (destination_0_amount as u64));")),
            "repeated token account should project owner bytes from a source-derived key; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_uses_abi_account_layout_for_symbolic_data_account() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
instruction UPDATE_CONFIG 9
account UPDATE_CONFIG CONFIG 0 writable

record CONFIG_ACCOUNT
field MAGIC bytes8
field ADMIN pubkey
field MAX_FEE_BPS u16
field PAUSED bool
end

record UPDATE_CONFIG_ARGS
field MAX_FEE_BPS u16
end

magic CONFIG_MAGIC CFGMAGIC
instruction_record UPDATE_CONFIG UPDATE_CONFIG_ARGS
account_record CONFIG CONFIG_ACCOUNT
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec ConfigProgram
state { max_fee_bps : U64 }
handler update_config (max_fee_bps : U64) {
  accounts {
    config : readonly
  }
  ensures state.max_fee_bps == old(state.max_fee_bps)
  effect { max_fee_bps := max_fee_bps }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("fn build_data_account"))
                && compact(&body).contains(&compact("// ABI account layout `config_account`: 43 byte data region."))
                && compact(&body).contains(&compact("let mut config_data: [u8; 43] = [0u8; 43];"))
                && compact(&body).contains(&compact("config_data[0] = 67u8;"))
                && compact(&body).contains(&compact("config_data[7] = 67u8;"))
                && compact(&body).contains(&compact("let mut config = build_data_account([1u8; 32], program_id, false, true, config_data);"))
                && compact(&body).contains(&compact("write_state_u16(&mut config, 40, (max_fee_bps as u16));"))
                && !compact(&body).contains(&compact("let mut config = build_minimal_account(")),
            "ABI account layouts should emit program-owned data accounts with profiled byte length and state witnesses; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_binds_profiled_pda_account_keys() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(
        program_root.join("src/state.rs"),
        r#"
pub fn derive_config(program_id: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[CONFIG_SEED], program_id)
}

pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[VAULT_AUTHORITY_SEED, &[lane_id]], program_id)
}
"#,
    )
    .unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
seed CONFIG_SEED config
seed VAULT_AUTHORITY_SEED vault-authority
instruction ROUTE 3
account ROUTE CONFIG 0 writable
account ROUTE VAULT_AUTHORITY 1

record ROUTE_ARGS
field LANE_ID u8
end

instruction_record ROUTE ROUTE_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec RouteProgram
state { dummy : U64 }
handler route (lane_id : U64) {
  accounts {
    config : writable
    vault_authority : readonly
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let config_key = crate::derive_config(&program_id).0;"))
                && compact(&body).contains(&compact("let vault_authority_key = crate::derive_vault_authority(&program_id, lane_id as u8).0;"))
                && compact(&body).contains(&compact("let mut config = build_minimal_account(config_key, false, true);"))
                && compact(&body).contains(&compact("let mut vault_authority = build_minimal_account(vault_authority_key, false, false);")),
            "profiled PDA derivations should bind exact account keys generically; got:\n{body}"
        );
    assert!(
            compact(&body).contains(&compact("/// - PDA derivations: config -> config (found); vault_authority -> vault_authority (found)")),
            "generated impl harness should report inferred PDA derivations; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_binds_account_keys_from_source_require_key_derivation() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(program_root.join("schema")).unwrap();
    std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::try_find_program_address(&[VAULT_AUTHORITY_SEED, &[lane_id]], program_id).unwrap()
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let vault = next_account_info(account_info_iter)?;
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let vault_key = derive_vault_authority(program_id, lane_id).0;
    require_key(vault, &vault_key)?;
    Ok(())
}
"#,
        )
        .unwrap();
    std::fs::write(
        program_root.join("schema/program.schema"),
        "seed VAULT_AUTHORITY_SEED vault-authority\n",
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec RouteProgram
state { dummy : U64 }
handler route (lane_id : U64) {
  accounts {
    vault : readonly
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
        compact(&body).contains(&compact(
            "let vault_key = crate::derive_vault_authority(&program_id, lane_id as u8).0;"
        )) && compact(&body).contains(&compact(
            "let mut vault = build_minimal_account(vault_key, false, false);"
        )),
        "source require_key derived-key guards should bind exact account keys; got:\n{body}"
    );
}

#[test]
fn pinocchio_impl_binds_non_program_id_pda_from_source_require_key_derivation() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];
pub const TOKEN_PROGRAM_ID: Pubkey = [9u8; 32];

pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(_program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], _instruction_data: &[u8]) -> ProgramResult {
    let [authority, mint, vault, ..] = accounts else {
        return Ok(());
    };
    require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
    Ok(())
}
"#,
        )
        .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec NonProgramPda
state { balance : U64 }
handler route (nonce : U64) {
  accounts {
    authority : readonly
    mint      : readonly
    vault     : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let vault_key = crate::derive_token_vault(&[1u8; 32], &[2u8; 32]).0;"))
                && compact(&body).contains(&compact("let mut vault = build_minimal_account(vault_key, false, true);")),
            "non-program-id PDA account keys should render from source require_key derivations; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_binds_non_program_id_pda_with_nested_derived_key_seed() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(program_root.join("schema")).unwrap();
    std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];

pub fn derive_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id)
}

pub fn derive_token_vault(program_id: &Pubkey, mint: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    let authority = derive_authority(program_id, lane_id).0;
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), crate::TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [mint, vault, ..] = accounts else {
        return Ok(());
    };
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let descriptor = VaultDescriptor {
        lane_id,
        mint: MintKey(*mint.key()),
    };
    require_key(
        vault,
        &derive_token_vault(program_id, &descriptor.mint.0, descriptor.lane_id.0).0,
    )?;
    Ok(())
}
"#,
        )
        .unwrap();
    std::fs::write(
        program_root.join("schema/program.schema"),
        "seed AUTHORITY_SEED authority\n",
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec NestedPda
state { balance : U64 }
handler route (lane_id : U64) {
  accounts {
    mint  : readonly
    vault : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let vault_authority_key = crate::derive_authority(&program_id, lane_id as u8).0;"))
                && compact(&body).contains(&compact("let vault_key = crate::derive_token_vault(&program_id, &[1u8; 32], lane_id as u8).0;"))
                && compact(&body).contains(&compact("let mut vault = build_minimal_account(vault_key, false, true);")),
            "nested derived-key PDA seeds should render before the outer non-program-id PDA; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_binds_repeated_loop_account_derivations_from_source() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(program_root.join("schema")).unwrap();
    std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];
pub const TOKEN_PROGRAM_ID: Pubkey = [9u8; 32];

pub fn derive_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id)
}

pub fn derive_token_vault(program_id: &Pubkey, mint: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    let authority = derive_authority(program_id, lane_id).0;
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), crate::TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let args = RouteArgs::try_from(instruction_data)?;
    let account_info_iter = &mut accounts.iter();
    let mint = next_account_info(account_info_iter)?;
    let route_mint = MintKey(*mint.key());
    for transfer in args.transfers {
        let source_vault = next_account_info(account_info_iter)?;
        let destination_vault = next_account_info(account_info_iter)?;
        require_key(
            source_vault,
            &derive_token_vault(program_id, &route_mint.0, transfer.from_lane_id.0).0,
        )?;
        require_key(
            destination_vault,
            &derive_token_vault(program_id, &route_mint.0, transfer.to_lane_id.0).0,
        )?;
    }
    Ok(())
}
"#,
        )
        .unwrap();
    std::fs::write(
        program_root.join("schema/program.schema"),
        r#"seed AUTHORITY_SEED authority
field FROM_LANE_ID u8
field TO_LANE_ID u8
record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
record ROUTE_ARGS
field TRANSFER_COUNT u8
repeat TRANSFER transfer 2 TRANSFER_COUNT
instruction ROUTE 3
instruction_record ROUTE ROUTE_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec RepeatedLoopPda
state { balance : U64 }
handler route_2
  (from_lane_id_0 : U64)
  (to_lane_id_0 : U64)
  (from_lane_id_1 : U64)
  (to_lane_id_1 : U64) {
  accounts {
    mint                  : readonly
    source_vault_0        : writable
    destination_vault_0   : writable
    source_vault_1        : writable
    destination_vault_1   : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let source_vault_0_authority_key = crate::derive_authority(&program_id, from_lane_id_0 as u8).0;"))
                && compact(&body).contains(&compact("let source_vault_0_key = crate::derive_token_vault(&program_id, &[1u8; 32], from_lane_id_0 as u8).0;"))
                && compact(&body).contains(&compact("let destination_vault_1_authority_key = crate::derive_authority(&program_id, to_lane_id_1 as u8).0;"))
                && compact(&body).contains(&compact("let destination_vault_1_key = crate::derive_token_vault(&program_id, &[1u8; 32], to_lane_id_1 as u8).0;"))
                && compact(&body).contains(&compact("let mut source_vault_0 = build_minimal_account(source_vault_0_key, false, true);"))
                && compact(&body).contains(&compact("let mut destination_vault_1 = build_minimal_account(destination_vault_1_key, false, true);")),
            "repeated loop account-key derivations should bind suffixed accounts from source; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_uses_source_profile_for_tag_accounts_and_payload_widths() {
    let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) (lane : U64) {
  accounts {
    source : writable
    destination : writable
    authority : signer
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
    let spec = parse_str(src).expect("parse");
    let dir = tempfile::tempdir().expect("tempdir");
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(src_dir.join("instructions")).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminant, data) = instruction_data.split_first().unwrap();
    match *discriminant {
        9 => instructions::move_tokens::process_move_tokens(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        src_dir.join("instructions/move_tokens.rs"),
        r#"
pub fn process_move_tokens(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [destination, authority, source, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let lane = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let amount = u64::from_le_bytes(
        instruction_data
            .get(1..9)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
    )
    .unwrap();

    let output = src_dir.join("kani_impl.rs");
    generate_from_spec(
        &spec,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
        compact(&body).contains(&compact("let instruction_tag: u8 = 9u8;")),
        "must use source-inferred dispatcher tag; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact(
            "/// - source account order: destination, authority, source"
        )) && compact(&body).contains(&compact("/// - ABI/dispatcher tag: 9"))
            && compact(&body).contains(&compact("/// - PDA derivations: none inferred")),
        "generated impl harness should explain profile facts and fallbacks; got:\n{body}"
    );
    let destination_pos = body
        .find("ManuallyDrop::new(account_info_from_stack(&mut destination))")
        .unwrap();
    let authority_pos = body
        .find("ManuallyDrop::new(account_info_from_stack(&mut authority))")
        .unwrap();
    let source_pos = body
        .find("ManuallyDrop::new(account_info_from_stack(&mut source))")
        .unwrap();
    assert!(
        destination_pos < authority_pos && authority_pos < source_pos,
        "must use source-inferred account order; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("instruction_data[1] = (lane as u8) as u8;"))
            && compact(&body).contains(&compact(
                "let generated_instruction_data_2_bytes = (amount as u64).to_le_bytes();"
            )),
        "must use source-inferred payload order and widths; got:\n{body}"
    );
}

#[test]
fn pinocchio_impl_keeps_non_trailing_unsupported_abi_fields_visible() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    let abi_root = workspace.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
instruction UPLOAD 12

record UPLOAD_ARGS
field MEMO bytes8
field AMOUNT u64
end

instruction_record UPLOAD UPLOAD_ARGS
"#,
    )
    .unwrap();

    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(
        &spec_path,
        r#"spec Upload
state { total : U64 }
handler upload (amount : U64) {
  accounts { payer : signer }
  ensures state.total == old(state.total)
  effect { total := total }
}"#,
    )
    .unwrap();

    let output = program_root.join("src/kani_impl.rs");
    generate(
        &spec_path,
        &output,
        /*explicit_flag=*/ true,
        Target::Pinocchio,
    )
    .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&output).unwrap();

    assert!(
            compact(&body).contains(&compact("let mut instruction_data = [0u8; 17];"))
                && compact(&body).contains(&compact("TODO: unsupported instruction field `memo` type `bytes8` at offset 1..9"))
                && compact(&body).contains(&compact("let generated_instruction_data_9_bytes = (amount as u64).to_le_bytes();")),
            "non-trailing unsupported ABI fields must keep absolute layout visible and pack later fields at the right offset; got:\n{body}"
        );
}

#[test]
fn pinocchio_token_delta_assertions_skip_aliasing_transfers() {
    let chained = vec![
        PinocchioTokenTransferAssertion {
            from: "account_a".to_string(),
            to: "account_b".to_string(),
            amount: "amount_0".to_string(),
        },
        PinocchioTokenTransferAssertion {
            from: "account_b".to_string(),
            to: "account_c".to_string(),
            amount: "amount_1".to_string(),
        },
    ];
    let mut body = String::new();
    emit_pinocchio_token_pre_snapshots(&mut body, &chained);
    emit_pinocchio_token_post_assertions(&mut body, &chained);
    assert!(
        compact(&body).contains(&compact("token transfer delta assertions skipped"))
            && !compact(&body).contains(&compact("assert_eq!(read_token_amount")),
        "chained transfers should not emit independent per-transfer final assertions; got:\n{body}"
    );

    let self_transfer = vec![PinocchioTokenTransferAssertion {
        from: "account_a".to_string(),
        to: "account_a".to_string(),
        amount: "amount".to_string(),
    }];
    let mut body = String::new();
    emit_pinocchio_token_pre_snapshots(&mut body, &self_transfer);
    emit_pinocchio_token_post_assertions(&mut body, &self_transfer);
    assert!(
        compact(&body).contains(&compact("token transfer delta assertions skipped"))
            && !compact(&body).contains(&compact("assert_eq!(read_token_amount")),
        "self-transfer aliases should not emit independent debit/credit assertions; got:\n{body}"
    );
}

#[test]
fn pinocchio_account_order_matches_normalized_names() {
    let src = r#"spec AccountOrder
state { total : U64 }
handler route {
  accounts {
    user_vault : signer
    token_program : program
    output_mint : readonly
  }
  ensures state.total == old(state.total)
  effect { total := total }
}"#;
    let spec = parse_str(src).expect("parse");
    let handler = &spec.handlers[0];
    let profile = PinocchioHandlerProfile {
        name: "route".to_string(),
        instruction_tag: None,
        accounts: vec![
            "outputMint".to_string(),
            "tokenProgram".to_string(),
            "userVault".to_string(),
        ],
        account_roles: BTreeMap::new(),
        token_account_bindings: BTreeMap::new(),
        mint_decimal_bindings: BTreeMap::new(),
        account_key_derivations: BTreeMap::new(),
        source_expr_aliases: BTreeMap::new(),
        verified_stubs: Vec::new(),
        params: Vec::new(),
        repeats: Vec::new(),
    };

    let ordered = pinocchio_account_order(handler, Some(&profile));
    let names: Vec<_> = ordered
        .iter()
        .map(|account| account.name.as_str())
        .collect();
    assert_eq!(names, ["output_mint", "token_program", "user_vault"]);
}

#[test]
fn pinocchio_profile_notes_explain_unusable_account_order() {
    let src = r#"spec AccountOrder
state { total : U64 }
handler route {
  accounts {
    user_vault : signer
    token_program : program
  }
  ensures state.total == old(state.total)
  effect { total := total }
}"#;
    let spec = parse_str(src).expect("parse");
    let handler = &spec.handlers[0];
    let profile = PinocchioHandlerProfile {
        name: "route".to_string(),
        instruction_tag: None,
        accounts: vec!["userVault".to_string(), "tokenProgramExtra".to_string()],
        account_roles: BTreeMap::new(),
        token_account_bindings: BTreeMap::new(),
        mint_decimal_bindings: BTreeMap::new(),
        account_key_derivations: BTreeMap::new(),
        source_expr_aliases: BTreeMap::new(),
        verified_stubs: Vec::new(),
        params: Vec::new(),
        repeats: Vec::new(),
    };

    let mut notes = String::new();
    emit_pinocchio_profile_notes(&mut notes, handler, None, Some(&profile));
    assert!(
            compact(&notes).contains(&compact("source account order: inferred order unusable; profile account `tokenProgramExtra` did not match spec accounts; using spec order")),
            "unusable inferred order should leave a generated breadcrumb; got:\n{notes}"
        );
}

#[test]
fn pinocchio_fee_normalization_equal_literal_decimals_uses_checked_threshold() {
    let src = r#"spec FeeSwap
state { max_fee_bps : U128 }
handler swap
  (amount_in : U64)
  (amount_out : U64)
  (max_fee_bps : U128)
  (input_decimals : U64)
  (output_decimals : U64)
  (fee_input_normalized : U128)
  (fee_output_normalized : U128) {
  accounts {
    input_mint  : readonly
    output_mint : readonly
  }
  requires amount_in > 0 else InvalidAmount
  requires amount_out > 0 else InvalidAmount
  requires max_fee_bps <= 10000 else InvalidFee
  requires input_decimals == 6 else InvalidMint
  requires output_decimals == 6 else InvalidMint
  requires fee_input_normalized == amount_in * 1000000000000 else InvalidAmount
  requires fee_output_normalized == amount_out * 1000000000000 else InvalidAmount
  ensures state.max_fee_bps == old(state.max_fee_bps)
}"#;
    let spec = parse_str(src).expect("parse");
    let handler = &spec.handlers[0];
    let mut mint_decimal_bindings = BTreeMap::new();
    mint_decimal_bindings.insert("input_mint".to_string(), "input_decimals".to_string());
    mint_decimal_bindings.insert("output_mint".to_string(), "output_decimals".to_string());
    let profile = PinocchioHandlerProfile {
        name: "swap".to_string(),
        instruction_tag: None,
        accounts: Vec::new(),
        account_roles: BTreeMap::new(),
        token_account_bindings: BTreeMap::new(),
        mint_decimal_bindings,
        account_key_derivations: BTreeMap::new(),
        source_expr_aliases: BTreeMap::new(),
        verified_stubs: Vec::new(),
        params: Vec::new(),
        repeats: Vec::new(),
    };

    let mut body = String::new();
    emit_pinocchio_fee_normalization_assumptions(&mut body, handler, Some(&profile));

    assert!(
            compact(&body).contains(&compact("let generated_fee_min_output = ((amount_in as u128) * generated_fee_retained_bps) / 10000u128;")) && compact(&body).contains(&compact("kani::assume((amount_out as u128) >= generated_fee_min_output);")),
            "equal literal decimals should cancel the shared normalization scale and emit the bounded fee floor; got:\n{body}"
        );
    assert!(
        !compact(&body).contains(&compact("generated_fee_input_normalized")),
        "equal literal decimals must not emit normalization-scale multiplication; got:\n{body}"
    );
}

#[test]
fn pinocchio_impl_emits_effect_only_state_harnesses_without_project_specifics() {
    let src = r#"spec ProjectSpecificConfig
state { max_fee_bps : U128 }
handler update_limit (new_max_fee_bps : U128) {
  accounts {
    config : writable, pda ["config"]
    admin  : signer
  }
  modifies [max_fee_bps]
  effect { max_fee_bps := new_max_fee_bps }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!(
        "kani_impl_project_config_{}.rs",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
            compact(&body).contains(&compact("fn verify_update_limit_impl"))
                && compact(&body).contains(&compact("kani::cover!(_result.is_ok()"))
                && compact(&body).contains(&compact("let program_id: [u8; 32] = [42u8; 32];"))
                && compact(&body).contains(&compact("crate::process_instruction(&program_id")),
            "effect-only handlers should emit generic state assertions without project-specific branches; got:\n{body}"
        );
}

#[test]
fn pinocchio_impl_declares_and_packs_pubkey_params() {
    let src = r#"spec PubkeyParam
state { dummy : U64 }
handler register (member : Pubkey) {
  accounts { config : writable }
  modifies [dummy]
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_pubkey_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
        .expect("Pinocchio kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();

    assert!(
        compact(&body).contains(&compact(
            "let member: [u8; 32] = kani::any(); // spec type: Pubkey"
        )),
        "Pubkey params must be declared as symbolic 32-byte arrays; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("instruction_data.extend_from_slice(&member);")),
        "Pubkey params must pack raw 32-byte values into instruction data; got:\n{body}"
    );
    assert!(
        !compact(&body).contains(&compact("TODO: declare symbolic param `member`"))
            && !compact(&body).contains(&compact("TODO: pack param `member`")),
        "Pubkey params should no longer fall through to TODOs; got:\n{body}"
    );
    let _ = std::fs::remove_file(&tmp);
}

/// `--kani-impl` flag explicitly forces emission for every handler
/// with ensures, regardless of the modifies-diff.
#[test]
fn explicit_flag_forces_emission_for_handlers_with_ensures() {
    let src = r#"spec ExplicitFlag
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
    let spec = parse_str(src).expect("parse");
    // Auto-trigger silent (no modifies declared).
    assert!(!spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_explicit_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("generate");
    assert!(tmp.is_file(), "explicit flag must emit the file");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        compact(&body).contains(&compact("fn verify_bump_impl_ensures_0()")),
        "explicit flag must emit per-handler harness; got:\n{}",
        body
    );
    assert!(
        compact(&body).contains(&compact("accounts.handler(delta)")),
        "harness must call the user's real handler; got:\n{}",
        body
    );
    let _ = std::fs::remove_file(&tmp);
}

/// PDA-derived account addresses bind to the spec-declared seeds
/// rather than `kani::any()`.
#[test]
fn pda_derived_accounts_bind_seed_expressions() {
    let src = r#"spec EscrowLite
state { initializer : Pubkey, amount : U64 }
pda escrow ["escrow", initializer]
handler open (deposit_amount : U64) {
  accounts {
    initializer : signer, writable
    escrow      : writable, pda ["escrow", initializer]
  }
  modifies [amount, initializer]
  ensures state.amount == deposit_amount
  effect { amount := deposit_amount }
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_pda_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        compact(&body).contains(&compact(
            "find_program_address(&[b\"escrow\", initializer.as_ref()]"
        )),
        "PDA derivation must come from the spec's `pda` declaration; got:\n{}",
        body
    );
    assert!(
        compact(&body).contains(&compact("`initializer`: signer")),
        "signer account must appear in the symbolic builder; got:\n{}",
        body
    );
    let _ = std::fs::remove_file(&tmp);
}

/// Issue #71: an integer handler param used as a PDA seed must
/// serialize via `to_le_bytes()` — `u64::as_ref()` does not exist.
/// Pubkey seeds / account keys keep `.as_ref()`.
#[test]
fn integer_param_seed_serializes_via_to_le_bytes() {
    let src = r#"spec Pool
state { lane_count : U64 }
pda vault_authority ["vault-authority", lane_id]
handler swap (lane_id : U64) {
  accounts {
    vault_authority : writable, pda ["vault-authority", lane_id]
    caller        : signer
  }
  modifies [lane_count]
  ensures state.lane_count == lane_id
  effect { lane_count := lane_id }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_intseed_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        compact(&body).contains(&compact("lane_id.to_le_bytes().as_ref()")),
        "integer-param seed must serialize via to_le_bytes; got:\n{}",
        body
    );
    assert!(
        !compact(&body).contains(&compact("[b\"vault-authority\", lane_id.as_ref()")),
        "must not emit bare `lane_id.as_ref()` for a u64 param; got:\n{}",
        body
    );
    let _ = std::fs::remove_file(&tmp);
}

/// No emit when neither the explicit flag is on NOR any handler
/// triggers auto-emission.
#[test]
fn no_emit_when_neither_flag_nor_auto_trigger() {
    let src = r#"spec Silent
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_silent_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    assert!(
        !tmp.is_file(),
        "no flag + no auto-trigger must skip file emission"
    );
}

// ========================================================================
// v2.26 Batch 2 Track I — CPI ensures-as-fact in impl-targeted harness
// ========================================================================

/// A handler with its own `ensures` AND a `call Iface.foo(args)` to an
/// interface that declares ensures must emit `kani::assume(...)` lines
/// between `if result.is_ok()` and the first caller `assert!`,
/// substituting the callee's param names with the caller's call-site
/// expressions. Mirror of `kani.rs`'s
/// `cpi_ensures_lowers_to_kani_assume_in_preservation_harness` for the
/// impl-targeted variant.
#[test]
fn cpi_ensures_as_assume_emits_at_splice_point() {
    let src = r#"spec CpiImplTest
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures amount > 0
  }
}

state { pool : U64 }

handler deposit (amt : U64) {
  permissionless
  requires amt > 0 else InvalidAmount
  modifies [pool, lp_supply]
  call Token.transfer(from = 0, to = 0, amount = amt, authority = 0)
  effect { pool += amt }
  ensures state.pool == old(state.pool) + amt
}"#;
    let spec = parse_str(src).expect("parse");
    // The LP-shape diff (modifies = {pool, lp_supply}, effect-LHS = {pool})
    // triggers auto-emission.
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_track_i_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    // 1. The splice-marker comment from Track H must be GONE — Track I
    //    replaces it with the actual emission, no stale marker.
    assert!(
        !compact(&body).contains(&compact("<Track I CPI ensures-as-fact splice point>")),
        "Track H's splice marker must be removed once Track I has emitted; got:\n{}",
        body
    );

    // 2. The CPI ensures-as-fact comment + assume line must be present,
    //    with `amount` substituted to the caller's `amt` expression.
    assert!(
        compact(&body).contains(&compact("// CPI ensures-as-fact (Token.transfer):")),
        "missing CPI ensures-as-fact comment for Token.transfer; got:\n{}",
        body
    );
    assert!(
        compact(&body).contains(&compact("kani::assume(amt > 0)")),
        "missing substituted kani::assume(amt > 0); got:\n{}",
        body
    );

    // 3. Ordering: assume must sit between `if result.is_ok()` and the
    //    caller's first `assert!`.
    let is_ok_pos = body
        .find("if result.is_ok()")
        .expect("harness must have `if result.is_ok()`");
    let assume_pos = body[is_ok_pos..]
        .find("kani::assume(amt > 0)")
        .map(|i| is_ok_pos + i)
        .expect("assume present (just asserted above)");
    let assert_pos = body[is_ok_pos..]
        .find("assert!")
        .map(|i| is_ok_pos + i)
        .expect("caller's assert! must follow");
    assert!(
        is_ok_pos < assume_pos && assume_pos < assert_pos,
        "CPI assume must sit between is_ok() and assert!; got:\n{}",
        body
    );

    let _ = std::fs::remove_file(&tmp);
}

/// v2.26 Track K — impl-targeted variant of the spec-model
/// `named_return_binder_substitutes_into_kani_assume` test.
/// `let p = call Oracle.quote(…)` with `-> price : U64` declared
/// must rewrite `price` to `p` in the emitted `kani::assume`.
#[test]
fn named_return_binder_substitutes_in_impl_harness() {
    let src = r#"spec NamedBinderImpl
program_id "11111111111111111111111111111111"

interface Oracle {
  program_id "11111111111111111111111111111111"
  handler quote (base : U64) -> price : U64 {
    ensures price > 0
  }
}

state { last_price : U64, lp_supply : U64 }

handler refresh (b : U64) {
  permissionless
  modifies [last_price, lp_supply]
  let p = call Oracle.quote(base = b)
  effect { last_price := b }
  ensures state.last_price == b
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_track_k_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    assert!(
        compact(&body).contains(&compact("// CPI ensures-as-fact (Oracle.quote):")),
        "missing CPI ensures-as-fact comment for Oracle.quote; got:\n{}",
        body,
    );
    // The callee uses `price` as its return binder; the caller's
    // `let p = …` makes `p` the substituted form.
    assert!(
        compact(&body).contains(&compact("kani::assume(p > 0)")),
        "expected `kani::assume(p > 0)` from named binder substitution; got:\n{}",
        body,
    );
    assert!(
        !compact(&body).contains(&compact("price > 0")),
        "binder name `price` must be substituted away; got:\n{}",
        body,
    );

    let _ = std::fs::remove_file(&tmp);
}

/// v2.27 Track A — `state_binders { ... }` rewrites
/// `pre.<callee_field>` / `post.<callee_field>` to the caller's
/// `pre.<caller_field>` / `post.<caller_field>` in the substituted
/// `kani::assume`, which then flattens through
/// `rewrite_pre_post_paths` to the harness-local
/// `pre_<caller_field>` / `post_<caller_field>` snapshots.
#[test]
fn state_binders_rewrite_through_impl_snapshot_locals() {
    let src = r#"spec StateBindersImpl
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures post.from_balance + amount == pre.from_balance
  }
}

state { pool_balance : U64, lp_supply : U64 }

handler deposit (amt : U64) {
  permissionless
  requires amt > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  call Token.transfer(
    from = 0,
    to = 0,
    amount = amt,
    authority = 0,
    state_binders { from_balance = state.pool_balance },
  )
  effect { pool_balance -=! amt }
  ensures state.pool_balance == old(state.pool_balance) - amt
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_track_a_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    // The substitution rewrites `pre.from_balance` /
    // `post.from_balance` → `pre.pool_balance` / `post.pool_balance`
    // (via state_binders), then `rewrite_pre_post_paths` flattens
    // those to the snapshot locals `pre_pool_balance` /
    // `post_pool_balance`.
    assert!(
        compact(&body).contains(&compact(
            "kani::assume(post_pool_balance + amt == pre_pool_balance)"
        )),
        "expected flat snapshot locals in kani::assume; got:\n{}",
        body,
    );
    // The callee abstract field name must NOT survive.
    assert!(
        !compact(&body).contains(&compact("from_balance")),
        "abstract callee field `from_balance` must be substituted; got:\n{}",
        body,
    );
    // The snapshot block must capture `pool_balance` (the caller-
    // side binder field) — otherwise `pre_pool_balance` /
    // `post_pool_balance` references the assume emits don't compile.
    assert!(
        compact(&body).contains(&compact("let pre_pool_balance")),
        "snapshot block must capture pre_pool_balance; got:\n{}",
        body,
    );
    assert!(
        compact(&body).contains(&compact("let post_pool_balance")),
        "snapshot block must capture post_pool_balance; got:\n{}",
        body,
    );

    let _ = std::fs::remove_file(&tmp);
}

/// Tier-0 callees (interface declares no `ensures`) must not emit any
/// `kani::assume` lines in the impl harness. Mirrors the spec-model
/// variant's `tier0_callee_emits_no_kani_assume_lines` test.
#[test]
fn tier0_callee_emits_no_kani_assume_lines() {
    let src = r#"spec Tier0Impl
program_id "11111111111111111111111111111111"

interface Logger {
  program_id "11111111111111111111111111111111"
  handler log (msg : U64) {
    accounts {
      sink : writable
    }
  }
}

state { counter : U64 }

handler tick (val : U64) {
  permissionless
  requires val > 0 else Bad
  modifies [counter, shadow]
  call Logger.log(msg = val)
  effect { counter += val }
  ensures state.counter == old(state.counter) + val
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_tier0_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    assert!(
        !compact(&body).contains(&compact("CPI ensures-as-fact (Logger.log)")),
        "Tier-0 callee (no ensures) must not emit any CPI assume block; got:\n{}",
        body
    );
    // Caller's own assert! still emits.
    assert!(
        compact(&body).contains(&compact("assert!(")),
        "caller's own assert! must still emit; got:\n{}",
        body
    );
    // And no `kani::assume(` introduced by Track I — the only assumes
    // that may appear are the caller's own requires-guard assume (none
    // here, since `val > 0` is the requires).
    // (We check by counting: the requires-guard assume is `val > 0`,
    // so a Logger-derived assume would appear separately.)
    let assume_count = body.matches("kani::assume(").count();
    // Exactly one assume — the caller's own requires-guard
    // (`val > 0 else Bad`).
    assert_eq!(
        assume_count, 1,
        "Tier-0 callee must not add any kani::assume lines; got {} assumes in:\n{}",
        assume_count, body
    );

    let _ = std::fs::remove_file(&tmp);
}

/// `let X = call Foo.bar(...)` puts `X` in scope in the substituted
/// ensures via the `result` convention (v2.24 #11). Mirrors the
/// spec-model variant's `let_call_binding_participates_in_substitution`
/// test.
#[test]
fn let_binding_participates_in_substitution() {
    let src = r#"spec LetCallImpl
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb (amount : U64) -> U64 {
    accounts {
      vault : writable
    }
    requires amount > 0
    ensures result <= amount
  }
}

state { total_loss : U64 }

handler liquidate (loss : U64) {
  permissionless
  requires loss > 0 else Bad
  modifies [total_loss, shadow]
  let burned = call Pool.absorb(amount = loss)
  effect { total_loss += loss }
  ensures state.total_loss == old(state.total_loss) + loss
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_letcall_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    assert!(
        compact(&body).contains(&compact("// CPI ensures-as-fact (Pool.absorb):")),
        "missing CPI ensures-as-fact for Pool.absorb; got:\n{}",
        body
    );
    // `result <= amount` substitutes `amount → loss` and
    // `result → burned`.
    assert!(
        compact(&body).contains(&compact("kani::assume(burned <= loss)")),
        "let-binding result must substitute to caller's binder; got:\n{}",
        body
    );

    let _ = std::fs::remove_file(&tmp);
}

/// v2.26 Track J — when `multi_cpi_shared_fields` fires for a
/// handler, the impl harness emits a WARNING breadcrumb comment
/// above the CPI assume block so a reader of the generated file
/// sees the over-constraint risk without cross-referencing the lint
/// output. The breadcrumb sits between the post-snapshot and the
/// first `kani::assume` from any CPI.
#[test]
fn multi_cpi_breadcrumb_emits_above_assume_block() {
    let src = r#"spec MultiCpiKaniImpl
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.vault_balance == old(state.vault_balance) - amount
  }
}

state { vault_balance : U64 }

handler split (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  modifies [vault_balance, shadow]
  call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
  call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
  effect { vault_balance -= a }
  ensures state.vault_balance == old(state.vault_balance) - a - b
}"#;
    let spec = parse_str(src).expect("parse");
    // LP-shape gap (modifies = {vault_balance, shadow}, effect-LHS =
    // {vault_balance}) triggers auto-emission.
    assert!(spec_triggers_impl_harness(&spec));

    let tmp = std::env::temp_dir().join(format!("kani_impl_multi_cpi_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor).expect("generate");
    let body = std::fs::read_to_string(&tmp).unwrap();

    // 1. Two CPI assume lines must be present (one per call).
    let assume_count = body
        .matches("// CPI ensures-as-fact (Token.transfer):")
        .count();
    assert_eq!(
        assume_count, 2,
        "two CPI assume blocks must emit; got {} in:\n{}",
        assume_count, body
    );

    // 2. The breadcrumb WARNING must appear in the harness body
    //    (above the CPI assume block).
    assert!(
        compact(&body).contains(&compact("WARNING: multi-CPI ordering")),
        "Track J breadcrumb must emit when multi_cpi_shared_fields fires; got:\n{}",
        body
    );
    assert!(
        compact(&body).contains(&compact("`multi_cpi_same_field`")),
        "breadcrumb must reference the lint rule name; got:\n{}",
        body
    );

    // 3. Ordering: WARNING sits between the `if result.is_ok()`
    //    branch open and the first `kani::assume` of the CPI block.
    let is_ok_pos = body
        .find("if result.is_ok()")
        .expect("`if result.is_ok()` must be present");
    let warn_pos = body
        .find("WARNING: multi-CPI ordering")
        .expect("breadcrumb present (just asserted)");
    let first_cpi_assume = body[is_ok_pos..]
        .find("// CPI ensures-as-fact")
        .map(|i| is_ok_pos + i)
        .expect("CPI assume block must follow is_ok()");
    assert!(
        is_ok_pos < warn_pos && warn_pos < first_cpi_assume,
        "WARNING breadcrumb must sit between is_ok() and the first \
             CPI ensures-as-fact comment; positions: is_ok={} warn={} cpi={}; got:\n{}",
        is_ok_pos,
        warn_pos,
        first_cpi_assume,
        body
    );

    let _ = std::fs::remove_file(&tmp);
}

/// v2.26 fold-in — a spec with no LP-shape handler but a ref_impl
/// that carries potentially-overflowing arithmetic still auto-triggers
/// the impl-targeted harness. Lean proves on `Nat`; Kani is the only
/// verification surface that catches the `u64` overflow.
#[test]
fn ref_impl_overflow_risk_auto_triggers_impl_harness() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl scaled (a : U64) (b : U64) : U64 = a * b

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
  ensures state.x == scaled(old(state.x), amt)
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    // No handler trips the LP-shape signal (`set` declares no modifies).
    assert!(
        !spec.handlers.iter().any(handler_triggers_impl_harness),
        "no handler should trip the modifies-driven trigger in this fixture"
    );
    // But the ref_impl `scaled` has `*` over U64, so the auto-trigger
    // still fires through the ref_impl overflow-risk predicate.
    assert!(
        spec_triggers_impl_harness(&spec),
        "ref_impl with multiplication over bounded-numeric params \
             must auto-trigger the impl harness"
    );
}

/// Symmetric negative: ref_impl with only division (no overflow risk)
/// AND no LP-shape handler — auto-trigger stays quiet.
#[test]
fn ref_impl_without_overflow_risk_does_not_auto_trigger() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl half (a : U64) : U64 = a / 2

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    assert!(
        !spec_triggers_impl_harness(&spec),
        "ref_impl with only division must not auto-trigger \
             (no overflow risk, nothing for Kani to catch)"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Context/instruction mode (#169)
// ────────────────────────────────────────────────────────────────────────────

/// The spec used across the Context-mode tests — mirrors the #169 de-risk
/// program (a `Settings` state account behind a `has_one`-style gate with a
/// signer): validated end-to-end under real `cargo kani` (E1–E5).
const CONTEXT_SPEC: &str = r#"spec CtxGate
pragma state_struct = Settings
pragma context_struct = set_threshold::Gate
state { admin : Pubkey, status : U8, threshold : U64 }
handler set_threshold (new_threshold : U64) {
  accounts {
    settings : writable
    admin : signer
  }
  requires state.status == 1 else NotActive
  modifies [threshold]
  ensures state.threshold == new_threshold
  effect { threshold := new_threshold }
}"#;

fn generate_context_mode(src: &str) -> String {
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!(
        "kani_impl_ctx_{}_{}.rs",
        std::process::id(),
        src.len()
    ));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Context,
    )
    .expect("context kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    body
}

/// #169: the Context harness drives the REAL `try_accounts` constraint gate —
/// leaked-backing AccountInfos, the pragma-named `Gate` struct, its generated
/// `GateBumps`, and the T3-escape-hatch deserialize stub wired to the
/// generated symbolic ctor.
#[test]
fn context_mode_emits_try_accounts_gate() {
    let body = generate_context_mode(CONTEXT_SPEC);

    assert!(
        compact(&body).contains(&compact("CONTEXT/instruction mode (#169)")),
        "header names the mode; got:\n{body}"
    );
    // The account-info plumbing + the real constraint gate call.
    assert!(
        compact(&body).contains(&compact("fn leak_account_info("))
            && compact(&body).contains(&compact(
                "<crate::Gate as anchor_lang::Accounts<_>>::try_accounts("
            ))
            && compact(&body).contains(&compact("let mut bumps = crate::GateBumps::default();")),
        "leaked AccountInfos + real try_accounts on the pragma-named struct; got:\n{body}"
    );
    // T3 escape hatch: try_deserialize stubbed to the generated symbolic ctor.
    assert!(
        compact(&body).contains(&compact("fn symbolic_settings() -> crate::Settings"))
            && compact(&body).contains(&compact("fn stub_try_deserialize_settings("))
            && compact(&body).contains(&compact("Ok(symbolic_settings())"))
            && compact(&body).contains(&compact(
                "#[kani::stub(crate::Settings::try_deserialize, stub_try_deserialize_settings)]"
            )),
        "deserialize stub wired to the symbolic ctor; got:\n{body}"
    );
    // The signer flag is SYMBOLIC and the signer-gate assert is GENERATED —
    // the crown-jewel "no unauthorized execution" property.
    assert!(
        compact(&body).contains(&compact("let admin_signer: bool = kani::any();"))
            && compact(&body).contains(&compact(
                "assert!(admin_signer, \"instruction succeeded without `admin`'s signature\");"
            )),
        "symbolic signer flag + generated signer-gate assert; got:\n{body}"
    );
    // Requires lowers to a pre-snapshot assume; ensures reads the deserialized
    // state account in place (`ctx_accounts.settings.<field>`).
    assert!(
        compact(&body).contains(&compact("let pre_status = ctx_accounts.settings.status;"))
            && compact(&body).contains(&compact("kani::assume(pre_status == 1);"))
            && compact(&body)
                .contains(&compact("ctx_accounts.settings.threshold == new_threshold")),
        "requires assume off the pre-snapshot; ensures reads ctx_accounts.<state>; got:\n{body}"
    );
    // The instruction-fn call is the ONE agent-fill site; non-vacuity cover
    // is generated.
    assert!(
        compact(&body).contains(&compact(
            "todo!(\"call the real instruction fn via Context::new\")"
        )) && compact(&body).contains(&compact(
            "kani::cover!(true, \"instruction success path reachable"
        )),
        "agent-fill instruction call + non-vacuity cover; got:\n{body}"
    );
}

/// Without `pragma context_struct`, the struct name defaults to the Anchor
/// convention: `PascalCase(handler)`.
#[test]
fn context_mode_defaults_struct_name_to_pascal() {
    let src = CONTEXT_SPEC.replace("pragma context_struct = set_threshold::Gate\n", "");
    let body = generate_context_mode(&src);
    assert!(
        compact(&body).contains(&compact(
            "<crate::SetThreshold as anchor_lang::Accounts<_>>::try_accounts("
        )) && compact(&body).contains(&compact("crate::SetThresholdBumps::default()")),
        "struct name defaults to PascalCase(handler); got:\n{body}"
    );
}

/// Program accounts get their well-known id (`token` → anchor_spl) and
/// `executable: true`; an unknown program id is a fail-loud `todo!()` (a
/// symbolic id would make `Program::try_accounts` always fail → vacuous).
#[test]
fn context_mode_program_accounts_well_known_or_fail_loud() {
    let src = r#"spec CtxProgs
pragma state_struct = Pool
state { total : U64 }
handler sweep {
  accounts {
    pool : writable
    admin : signer
    token_program : program, type token
    oracle_program : program
  }
  modifies [total]
  ensures state.total == 0
  effect { total := 0 }
}"#;
    let body = generate_context_mode(src);
    assert!(
        compact(&body).contains(&compact("anchor_spl::token::ID")),
        "token program uses its well-known id; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("todo!(\"real program id for `oracle_program`\")")),
        "unknown program id is fail-loud, never symbolic; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("/*executable=*/ true")),
        "program accounts are executable; got:\n{body}"
    );
}

/// #179(c): `old()` over a NESTED field path (`old(state.window.remaining)`)
/// lowers correctly in the brownfield harness — the parent record is
/// snapshotted (cloned, non-`Copy`), the requires/ensures read
/// `pre_window.remaining`, and the post side reads the mutated
/// `state.window.remaining` in place. Pins the method-postcondition
/// arithmetic shape from the Squads migration (G15).
#[test]
fn brownfield_old_over_nested_field_path() {
    let src = r#"spec NestedOld
pragma state_struct = SpendingLimit
type Window = { remaining : U64, resets_at : I64 }
state { authority : Pubkey, window : Window }
handler decrement (amount : U64) {
  requires amount <= state.window.remaining else Exceeded
  modifies [window]
  ensures state.window.remaining == old(state.window.remaining) - amount
  effect { window.remaining := window.remaining - amount }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_nold_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("let pre_window = state.window.clone();")),
        "nested-old parent record snapshotted via clone; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("kani::assume(amount <= pre_window.remaining);")),
        "requires reads the nested pre-snapshot path; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact(
            "state.window.remaining == pre_window.remaining - amount"
        )),
        "ensures compares post in-place read vs nested pre-snapshot; got:\n{body}"
    );
}

/// #163/G2: `pragma kani_target = <handler>::<method>` mechanizes the effect
/// call — the ensures/reject harnesses bind `ok` to the REAL state-struct
/// method call (`.is_ok()` for the default `result` kind) and the panic-free
/// harness calls it as a statement. No AGENT-FILL effect site remains.
#[test]
fn kani_target_mechanizes_effect_call() {
    let src = r#"spec Targeted
pragma state_struct = SpendingLimit
pragma kani_target = decrement::try_decrement
pragma kani_reject = on
pragma kani_panic_free = on
state { remaining : U64 }
handler decrement (amount : U64) {
  requires amount <= state.remaining else Exceeded
  modifies [remaining]
  ensures state.remaining == old(state.remaining) - amount
  effect { remaining := remaining - amount }
}"#;
    let spec = parse_str(src).expect("parse");
    let tmp = std::env::temp_dir().join(format!("kani_impl_tgt_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // Ensures + reject harnesses: the ok-binding is the generated method call.
    assert_eq!(
        body.matches("let ok: bool = state.try_decrement(amount).is_ok();")
            .count(),
        2,
        "ensures + reject bind ok to the generated call; got:\n{body}"
    );
    // Panic-free harness: statement call, `let _ =` to swallow #[must_use].
    assert!(
        compact(&body).contains(&compact("let _ = state.try_decrement(amount);")),
        "panic-free calls the target as a statement; got:\n{body}"
    );
    // No agent-fill effect todo remains anywhere (the header PROSE mentions
    // `todo!()` as the fallback; only real call sites carry a message string).
    assert!(
        !compact(&body).contains(&compact("todo!(\"")),
        "kani_target leaves NO agent-fill todo; got:\n{body}"
    );
}

/// #163: the optional third segment maps the return shape — `bool` gates on
/// the value directly, `unit` treats a non-panicking return as success.
#[test]
fn kani_target_kind_segment_maps_return_shape() {
    let base = r#"spec Targeted
pragma state_struct = Gauge
pragma kani_target = poke::poke_impl::KIND
state { n : U64 }
handler poke (v : U64) {
  modifies [n]
  ensures state.n == v
  effect { n := v }
}"#;
    for (kind, expect) in [
        ("bool", "let ok: bool = state.poke_impl(v);"),
        ("unit", "state.poke_impl(v);\n    let ok: bool = true;"),
    ] {
        let spec = parse_str(&base.replace("KIND", kind)).expect("parse");
        let tmp =
            std::env::temp_dir().join(format!("kani_impl_tgtk_{}_{kind}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec_with_mode(
            &spec,
            &tmp,
            /*explicit_flag=*/ true,
            Target::Anchor,
            KaniImplMode::Brownfield,
        )
        .expect("brownfield kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(
            compact(&body).contains(&compact(expect)),
            "kind `{kind}` maps the return shape; got:\n{body}"
        );
    }
}

/// #191: `Bytes64` anywhere in state/params forces the 66 unwind floor — a
/// raw `[u8; 64]` compare is a 64-iteration memcmp loop that Kani cannot
/// stub (generic core impl, kani#1997), so the bound must cover it even
/// though the Pubkey abstraction is active for the Pubkey field.
#[test]
fn bytes64_forces_unwind_floor_over_pubkey_abstraction() {
    let src = r#"spec SigRegistry
pragma state_struct = Registry
state { authority : Pubkey, last_sig : Bytes64, nonce : U64 }
handler record_sig (sig : Bytes64) {
  modifies [last_sig, nonce]
  ensures state.nonce == old(state.nonce) + 1
  effect {
    last_sig := sig
    nonce += 1
  }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_b64_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("#[kani::unwind(66)]")),
        "Bytes64 present → 66 floor, NOT the abstracted-Pubkey small bound; got:\n{body}"
    );
    // The symbolic ctor builds the raw array directly — no Pubkey wrapper.
    assert!(
        compact(&body).contains(&compact("last_sig: kani::any(),")),
        "Bytes64 field constructs as a plain `kani::any()` array; got:\n{body}"
    );
}

/// #191: `Bytes32` (hash/digest) keeps the 34 memcmp floor; without any byte
/// token the same spec closes at the abstracted small bound (control is
/// covered by existing unwind tests).
#[test]
fn bytes32_forces_unwind_34() {
    let src = r#"spec MerkleGate
pragma state_struct = Gate
state { root : Bytes32, epoch : U64 }
handler set_root (new_root : Bytes32) {
  modifies [root, epoch]
  ensures state.epoch == old(state.epoch) + 1
  effect {
    root := new_root
    epoch += 1
  }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_b32_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact("#[kani::unwind(34)]")),
        "Bytes32 present → 34 memcmp floor; got:\n{body}"
    );
}

/// #189: `pragma kani_stub_pda` emits the deterministic uninterpreted-function
/// PDA stub (UfMap-backed, `find` + `create` domains) and the unwind bound
/// covers the CAP=8 memo scan even for a numeric-only spec.
#[test]
fn pda_stub_is_deterministic_ufmap() {
    let src = r#"spec VaultInit
pragma state_struct = Vault
pragma kani_stub_pda = derive
state { total : U64 }
handler deposit (amount : U64) {
  requires amount > 0 else BadAmount
  modifies [total]
  ensures state.total == old(state.total) + amount
  effect { total += amount }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_ufpda_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    assert!(
        compact(&body).contains(&compact(
            "static QEDGEN_PDA_UF: qedgen_kani_prelude::UfCell32<8>"
        )) && compact(&body).contains(&compact("fn find_pda_abstract("))
            && compact(&body).contains(&compact("fn create_pda_abstract(")),
        "PDA stub is the UfMap-backed deterministic pair; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("#[kani::stub(solana_program::pubkey::Pubkey::find_program_address, find_pda_abstract)]"))
            && compact(&body).contains(&compact("#[kani::stub(solana_program::pubkey::Pubkey::create_program_address, create_pda_abstract)]")),
        "both PDA entry points are stubbed; got:\n{body}"
    );
    assert!(
        compact(&body).contains(&compact("#[kani::unwind(10)]")),
        "UfMap memo scan floors the unwind at 10; got:\n{body}"
    );
}

/// #189: `pragma kani_stub_hash` / `pragma kani_stub_secp256k1` emit the
/// per-primitive uninterpreted-function stubs + their `#[kani::stub]` attrs.
#[test]
fn hash_and_secp256k1_stub_pragmas_emit() {
    let src = r#"spec HashGate
pragma state_struct = Gate
pragma kani_stub_hash = on
pragma kani_stub_secp256k1 = on
state { total : U64 }
handler bump (amount : U64) {
  modifies [total]
  ensures state.total == old(state.total) + amount
  effect { total += amount }
}"#;
    let spec = parse_str(src).expect("parse");

    let tmp = std::env::temp_dir().join(format!("kani_impl_hash_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    generate_from_spec_with_mode(
        &spec,
        &tmp,
        /*explicit_flag=*/ true,
        Target::Anchor,
        KaniImplMode::Brownfield,
    )
    .expect("brownfield kani_impl must emit");
    let body = std::fs::read_to_string(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);

    // One UfMap per hash primitive (domain separation) + hash/hashv agreement
    // by shared key construction.
    for needle in [
        "static QEDGEN_SHA256_UF:",
        "static QEDGEN_KECCAK_UF:",
        "static QEDGEN_BLAKE3_UF:",
        "#[kani::stub(solana_program::hash::hash, sha256_abstract)]",
        "#[kani::stub(solana_program::hash::hashv, sha256v_abstract)]",
        "#[kani::stub(solana_program::keccak::hash, keccak_abstract)]",
        "#[kani::stub(solana_program::blake3::hash, blake3_abstract)]",
        "static QEDGEN_SECP_UF: qedgen_kani_prelude::UfCell64<8>",
        "#[kani::stub(solana_program::secp256k1_recover::secp256k1_recover, secp256k1_recover_abstract)]",
    ] {
        assert!(compact(&body).contains(&compact(needle)), "missing `{needle}` in:\n{body}");
    }
}

/// #192: a property that reads into a `Vec` state field with the bound at its
/// silent default warns (naming the field + pragma); an explicit
/// `kani_vec_bound` or a scalar-only property stays silent.
#[test]
fn vec_bound_undercoverage_lint() {
    // (a) ensures reads the Vec field, bound unset → warns.
    let reads = r#"spec Council
pragma state_struct = Council
type Member = { key : Pubkey }
state { members : Vec Member, quorum : U16 }
handler noop (x : U16) {
  modifies [quorum]
  ensures state.members == old(state.members)
  effect { quorum := x }
}"#;
    let spec = parse_str(reads).expect("parse");
    let warnings = state_ctor::vec_bound_undercoverage_warnings(&spec);
    assert_eq!(warnings.len(), 1, "one warning per Vec field: {warnings:?}");
    assert!(
        warnings[0].contains("members") && warnings[0].contains("kani_vec_bound"),
        "warning names the field and the pragma; got: {}",
        warnings[0]
    );

    // (b) explicit bound (even a low one) → conscious trade-off, silent.
    let bounded = reads.replace(
        "pragma state_struct = Council",
        "pragma state_struct = Council\npragma kani_vec_bound = 1",
    );
    let spec = parse_str(&bounded).expect("parse");
    assert!(
        state_ctor::vec_bound_undercoverage_warnings(&spec).is_empty(),
        "explicit kani_vec_bound silences the lint"
    );

    // (c) scalar-only property over a spec WITH a Vec field → silent (the
    // default-1 trade-off is exactly right there).
    let scalar = r#"spec Council
pragma state_struct = Council
type Member = { key : Pubkey }
state { members : Vec Member, quorum : U16 }
handler noop (x : U16) {
  modifies [quorum]
  ensures state.quorum == x
  effect { quorum := x }
}"#;
    let spec = parse_str(scalar).expect("parse");
    assert!(
        state_ctor::vec_bound_undercoverage_warnings(&spec).is_empty(),
        "scalar-only ensures stays silent"
    );
}

/// Whitespace-insensitive needle match: generated Rust is rustfmt-formatted
/// at the write seam, so tests must not depend on line wrapping.
fn compact(s: &str) -> String {
    let squeezed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    // rustfmt adds trailing commas when it wraps a call/struct/array across
    // lines; normalize them away so needles match either rendering.
    squeezed
        .replace(",)", ")")
        .replace(",]", "]")
        .replace(",}", "}")
}
