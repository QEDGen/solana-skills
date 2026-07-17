# Phase-2 interview examples — `AskUserQuestion` transcripts

These are example `AskUserQuestion` transcripts the auditor fires in
Phase 2 of an audit (after the first reproducible MED+ has surfaced —
see `workflow_walkthrough.md`). The agent has internal invariant /
state-machine / authority-graph / threat-model hypotheses from Phase
1's read pass; this file shows how to surface them as TUI questions
for user ratification.

Each transcript shows the same four-batch shape:

1. **Invariants** (multi-select) — 5–7 structural and domain candidates
   with code `preview` per option. Domain candidates state units, timing,
   rounding, or economic relationships rather than generic labels alone.
2. **State machine shape** (single-select) — archetype with struct +
   handler-signature preview.
3. **Authority graph** (multi-select) — extracted roles with `Signer`
   constraint preview per option.
4. **Threat scenarios + intentional gaps** (mixed: single-select on
   attacker model, multi-select on shortcuts).

Question shape matches the harness `AskUserQuestion` tool: each
question has `question`, `header` (≤12 chars), `multiSelect: bool`,
and `options: [{label, description, preview?}, …]`. JSON below is
compact for the doc; the agent constructs the array client-side.

---

## Transcript 1 — Authority-gated vault (one-shot init, single admin)

**Scene:** vault program with admin-controlled fee parameters. Single
`State` PDA, three handlers (`initialize`, `update_fee`, `withdraw`).

**Phase 1 produced:**
- MED — `arithmetic_no_overflow` in `withdraw`: `amount * fee_bps / 10_000`
  overflows for `amount ≥ 2^56` (Mollusk repro fired).
- LOW — `account_owner_check` missing on `fee_recipient`.

### Batch 1 — Invariants

```json
{ "question": "Which invariants must always hold?", "header": "Invariants", "multiSelect": true, "options": [
  { "label": "fee_bps ≤ 10_000", "description": "Fee bps never exceed 100%.",
    "preview": "pub struct State { pub admin: Pubkey, pub fee_bps: u16, pub vault_balance: u64, pub initialized: bool }" },
  { "label": "vault_balance == Σ deposits − Σ withdrawals", "description": "Conservation: on-chain balance matches accounting.",
    "preview": "// withdraw: state.vault_balance = state.vault_balance.checked_sub(amount)?;" },
  { "label": "initialized is monotonic (false → true, never back)", "description": "One-shot init.",
    "preview": "require!(!ctx.accounts.state.initialized, ErrorCode::AlreadyInit);" },
  { "label": "admin pubkey is immutable after init", "description": "No handler rotates admin.",
    "preview": "// admin = assigned only in initialize(); no update_admin handler" },
  { "label": "withdraw amount ≤ vault_balance", "description": "No over-withdrawal.",
    "preview": "require!(amount <= state.vault_balance, ErrorCode::Insufficient);" },
  { "label": "fee_recipient mint == vault mint", "description": "Mint match on transfers.",
    "preview": "// (no constraint found; auditor-inferred candidate)" },
  { "label": "Other (free text)", "description": "Add an invariant the auditor missed." }
] }
```

**User clicks:** 1, 2, 3, 5. (Skips immutable-admin: rotation handler
planned. Skips mint-match: enforced upstream.)

**Phase 3 effect:** Producer A re-prioritizes overflow / underflow
probes against the ratified `vault_balance` conservation; every effect
touching `vault_balance` must preserve it under `amount ≤ balance`.
Unratified mint-match dropped from spec-candidate emission.

Before emitting the spec, the agent also resolves what `vault_balance` means:
book value, measured token-account balance, or net assets after accrued fees.
That semantic choice belongs in the domain layer; the arithmetic guard alone is
only a regression property.

### Batch 2 — State machine shape

```json
{ "question": "Which lifecycle shape matches this program's State?", "header": "Lifecycle", "multiSelect": false, "options": [
  { "label": "One-shot init", "description": "Initialize once; no teardown / re-init.",
    "preview": "pub struct State { ..., pub initialized: bool }\n\nfn initialize(...) -> Result<()>\nfn update_fee(...) -> Result<()>\nfn withdraw(...) -> Result<()>" },
  { "label": "Monotonic lifecycle (init → active → closed)", "description": "Ordered stages; never backwards." },
  { "label": "Oscillating state (e.g. paused ↔ active)", "description": "Reversible transitions." },
  { "label": "Free-form / no lifecycle", "description": "Handlers operate independently." }
] }
```

**User clicks:** "One-shot init."

**Phase 3 effect:** auditor asserts `initialized` monotonicity; any
handler whose effect can clear `initialized` becomes a fired finding.

### Batch 3 — Authority graph

```json
{ "question": "Which authority roles exist?", "header": "Authorities", "multiSelect": true, "options": [
  { "label": "admin — sole authority for parameter updates", "description": "Signs fee/config changes.",
    "preview": "#[account(mut, has_one = admin)] pub state: Account<'info, State>,\npub admin: Signer<'info>," },
  { "label": "user — unprivileged signer for withdraw", "description": "Any signer can withdraw their own funds.",
    "preview": "fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> { /* no admin signer required */ }" },
  { "label": "fee_recipient — receiving wallet, doesn't sign", "description": "Passive role.",
    "preview": "pub fee_recipient: Account<'info, TokenAccount>," }
] }
```

**User clicks:** admin + user.

**Phase 3 effect:** Producer B's authority sweep treats `withdraw` as
intentionally permissionless and stops emitting "missing signer"
candidates against it. Any handler mutating `fee_bps` without
`admin: Signer` becomes a HIGH finding.

### Batch 4 — Threat scenarios + intentional gaps

```json
[
  { "question": "What attacker model?", "header": "Threat", "multiSelect": false, "options": [
    { "label": "Unprivileged attacker, no signers", "description": "Worst-case caller has no admin / authority signature." },
    { "label": "Compromised user signer", "description": "Attacker can sign as any individual user." },
    { "label": "Compromised authority", "description": "Attacker controls the admin keypair." },
    { "label": "No specific threat model", "description": "Pre-launch hardening; surface anything." }
  ] },
  { "question": "Intentional shortcuts not to flag?", "header": "Shortcuts", "multiSelect": true, "options": [
    { "label": "Admin can rug (drain via fee_bps = 10_000)", "description": "Privileged role trusted by design." },
    { "label": "No reentrancy guard on CPI to token program", "description": "Trusting upstream isolation." },
    { "label": "No price-oracle staleness check", "description": "Out of scope." }
  ] }
]
```

**User clicks:** attacker = "Unprivileged attacker, no signers";
shortcuts = "Admin can rug."

**Phase 3 effect:** admin-rug shortcut suppresses `fee_bps = 10_000`
as a finding. Producer A focuses remaining budget on no-signer paths
— `withdraw` arithmetic, anonymous reentrancy, PDA derivation,
owner-check on `fee_recipient`.

---

## Transcript 2 — Token-program-like (three authorities, no lifecycle)

**Scene:** SPL-token-shaped program. Each `Mint` carries optional
`mint_authority`, `freeze_authority`, `close_authority`. Handlers:
`mint_to`, `burn`, `transfer`, `freeze_account`, `thaw_account`,
`close_account`, `set_authority`.

**Phase 1 produced:**
- HIGH — `account_signer_check` mismatch on `set_authority`: handler
  reads `current_authority` from the account but doesn't enforce it
  as `Signer<'info>` (Miri repro fired).

### Batch 1 — Invariants

```json
{ "question": "Which invariants must hold?", "header": "Invariants", "multiSelect": true, "options": [
  { "label": "supply == Σ account.amount across this mint", "description": "Conservation across mint/burn/transfer.",
    "preview": "pub struct Mint { pub supply: u64, pub mint_authority: COption<Pubkey>, ... }" },
  { "label": "freeze_authority == None ⇒ no account can be frozen", "description": "Disabled-by-construction freeze.",
    "preview": "if mint.freeze_authority.is_none() { return err!(NoFreezeAuthority); }" },
  { "label": "set_authority(MintTokens) requires current mint_authority as signer", "description": "Authority transitions signed by current holder.",
    "preview": "fn set_authority(ctx: Context<SetAuthority>, ty: AuthorityType, new: COption<Pubkey>) -> Result<()>" },
  { "label": "mint_authority cleared to None is permanent", "description": "Disabled mint is one-way.",
    "preview": "// pub mint_authority: COption<Pubkey>" },
  { "label": "Token amount never underflows (u64)", "description": "Burn/transfer cannot underflow.",
    "preview": "account.amount = account.amount.checked_sub(amount).ok_or(...)?;" },
  { "label": "close_account requires amount == 0", "description": "Cannot close non-empty account.",
    "preview": "require!(account.amount == 0, ErrorCode::NonEmpty);" },
  { "label": "Other (free text)", "description": "Add what auditor missed." }
] }
```

**User clicks:** all six.

**Phase 3 effect:** Producer A schedules conservation harness across
every (`mint_to`, `burn`, `transfer`) tuple under the ratified supply
invariant; the disabled-freeze invariant becomes a fired-finding
target if any handler can set `freeze_authority` non-None after clear.

### Batch 2 — State machine shape

```json
{ "question": "Which lifecycle best matches each Mint?", "header": "Lifecycle", "multiSelect": false, "options": [
  { "label": "One-shot init", "description": "Mint created once and never changes shape." },
  { "label": "Monotonic lifecycle — capabilities only ratchet down", "description": "Authorities can be revoked (→ None) but not granted from None.",
    "preview": "pub struct Mint { pub mint_authority: COption<Pubkey>, pub freeze_authority: COption<Pubkey>, pub close_authority: COption<Pubkey> }\n// set_authority: Some → Some, Some → None; never None → Some" },
  { "label": "Oscillating state", "description": "Authorities can flip back and forth." },
  { "label": "Free-form", "description": "No coherent state machine." }
] }
```

**User clicks:** "Monotonic lifecycle — capabilities only ratchet down."

**Phase 3 effect:** auditor asserts each `*_authority` field as a
monotone-revocation lattice (`Some(_) ⊑ None`). Any code path mapping
`None → Some(_)` becomes a CRIT-shaped target — for `MintTokens`,
`FreezeAccount`, and `CloseAccount` alike.

### Batch 3 — Authority graph

```json
{ "question": "Which authority roles exist?", "header": "Authorities", "multiSelect": true, "options": [
  { "label": "mint_authority — can mint new tokens", "description": "Signs MintTo.",
    "preview": "#[account(constraint = mint.mint_authority == COption::Some(authority.key()))]\npub mint: Account<'info, Mint>, pub authority: Signer<'info>," },
  { "label": "freeze_authority — can freeze/thaw token accounts", "description": "Signs Freeze/Thaw.",
    "preview": "fn freeze_account(...) -> Result<()> { /* checks mint.freeze_authority == authority.key() */ }" },
  { "label": "close_authority — can close a token account", "description": "Signs CloseAccount.",
    "preview": "fn close_account(...) -> Result<()> { /* checks account.close_authority */ }" },
  { "label": "account_owner — transfer/approve out of own account", "description": "Per-account owner signer.",
    "preview": "#[account(mut, has_one = owner)] pub source: Account<'info, TokenAccount>, pub owner: Signer<'info>," },
  { "label": "delegate — approved-amount-bounded transfer authority", "description": "Signs transfers up to approved amount.",
    "preview": "if account.delegate == COption::Some(authority.key()) && account.delegated_amount >= amount { ... }" }
] }
```

**User clicks:** all five.

**Phase 3 effect:** Producer B builds a 5-row authority × handler
matrix; any handler whose effect doesn't consume the ratified
authority for that row becomes an authority-confusion candidate. The
fired HIGH on `set_authority` maps to row 1.

### Batch 4 — Threat scenarios + intentional gaps

```json
[
  { "question": "Which attacker model?", "header": "Threat", "multiSelect": false, "options": [
    { "label": "Unprivileged attacker, no signers", "description": "Most paranoid; zero captured keys." },
    { "label": "Compromised user signer", "description": "Attacker holds arbitrary token-account owner key." },
    { "label": "Compromised authority", "description": "Attacker holds a mint/freeze/close authority key." },
    { "label": "No specific threat model" }
  ] },
  { "question": "Intentional shortcuts not to flag?", "header": "Shortcuts", "multiSelect": true, "options": [
    { "label": "mint_authority can inflate supply arbitrarily", "description": "By design." },
    { "label": "close_authority can withdraw rent", "description": "Rent goes to close_authority destination by design." },
    { "label": "Delegate-amount race (front-running approve/transfer)", "description": "Known issue, out of scope." }
  ] }
]
```

**User clicks:** attacker = "Compromised user signer"; shortcuts =
"mint_authority can inflate supply" + "Delegate-amount race."

**Phase 3 effect:** Producer A upgrades `account_owner`-gated probes
to "fired = HIGH at minimum" — an attacker who holds a user key can
already do the legitimate thing; the audit cares about what they
shouldn't. The delegate-race shortcut suppresses
`arithmetic_no_overflow` on `delegated_amount`.

---

## Transcript 3 — Lifecycle-heavy position state machine

**Scene:** lending-protocol-like program. Each user `Position` moves
through `Pending → Active → {Closed, Liquidated}`. Handlers:
`open_position`, `add_collateral`, `borrow`, `repay`, `close_position`,
`liquidate`. 4-variant `PositionState`; multiple handler-arrow paths.

**Phase 1 produced:**
- MED — `state_machine_no_skip` on `liquidate`: handler can transition
  `Pending → Liquidated` directly, skipping `Active`. Mollusk repro
  shows a never-funded `Pending` position "liquidated" with zero
  seizure (state corruption, no value extraction yet).

This transcript shows the state-machine question **enriched** —
because the agent saw a 4-variant enum, the option list presents
candidate transition graphs rather than the fixed archetype list.

### Batch 1 — Invariants

```json
{ "question": "Which invariants must hold?", "header": "Invariants", "multiSelect": true, "options": [
  { "label": "collateral * COLL_FACTOR ≥ debt (solvency)", "description": "Position over-collateralized while Active.",
    "preview": "pub struct Position { pub state: PositionState, pub collateral: u64, pub debt: u64, ... }\n// borrow: require!(position.collateral * COLL_FACTOR >= position.debt + amount * price);" },
  { "label": "Σ(position.debt over Active) == pool.total_borrowed", "description": "Pool-level conservation of borrows.",
    "preview": "// borrow: pool.total_borrowed = pool.total_borrowed.checked_add(amount)?;" },
  { "label": "Σ(position.collateral) == pool.total_collateral", "description": "Pool-level conservation of collateral." },
  { "label": "liquidation seizes ≥ debt-equivalent in collateral", "description": "Liquidator receives ≥ 1.0× debt (often + bonus).",
    "preview": "let seize = position.debt * (LIQ_BONUS + ONE) / price; transfer(seize);" },
  { "label": "Transitions follow Pending → Active → {Closed, Liquidated} only", "description": "No skips, no backward.",
    "preview": "pub enum PositionState { Pending, Active, Closed, Liquidated }" },
  { "label": "Active → Closed only when debt == 0", "description": "Cannot close with outstanding debt.",
    "preview": "require!(position.debt == 0, ErrorCode::OutstandingDebt);" },
  { "label": "Other (free text)" }
] }
```

The agent must normalize units before ratification: whether `collateral` is raw
atoms or oracle-valued quote units, the scale of `price`, and whether accrued
interest is included in `debt`. If source does not establish these semantics,
the preview presents the alternatives rather than silently selecting one.

**User clicks:** all six.

**Phase 3 effect:** Producer A schedules Kani harnesses for solvency
on every handler mutating `collateral` or `debt` (4-handler × 2-field
= 8 obligations). Producer B runs the pool-conservation cross-handler
sweep. The Phase-1 MED maps directly to "transitions follow … only."

### Batch 2 — State machine shape (enriched options)

```json
{ "question": "Which transition graph is intended?", "header": "Lifecycle", "multiSelect": false, "options": [
  { "label": "Pending → Active → Closed; Active → Liquidated", "description": "Open → Pending; first deposit activates; repay-to-zero closes; under-collateral mid-Active can be liquidated.",
    "preview": "pub enum PositionState { Pending, Active, Closed, Liquidated }\nfn open_position(...)  // → Pending\nfn add_collateral(...) // Pending → Active (first deposit) or Active → Active\nfn borrow(...)         // Active → Active\nfn repay(...)          // Active → Active or → Closed (if debt cleared)\nfn liquidate(...)      // Active → Liquidated\nfn close_position(...) // Active → Closed" },
  { "label": "Pending → {Active, Closed}; Active → {Closed, Liquidated}", "description": "Also allows abandoning a never-funded Pending via close_position.",
    "preview": "// same struct; close_position permitted from Pending iff collateral == 0" },
  { "label": "Pending → Active ↔ Frozen; Active → {Closed, Liquidated}", "description": "Includes admin-pausable Frozen state." },
  { "label": "Free-form / handler-driven", "description": "No fixed transition graph." }
] }
```

**User clicks:** "Pending → {Active, Closed}; Active → {Closed,
Liquidated}." (Confirms abandoning a `Pending` via `close_position`
is intended; `liquidate` from `Pending` is **not** — confirming the
Phase-1 MED.)

**Phase 3 effect:** Producer A locks the transition graph as the
ratified state-machine spec; any handler whose effect produces a
non-edge transition becomes HIGH-or-CRIT. The Phase-1 MED auto-
promotes (intent gap now confirmed) and the report-line text shifts
from "*candidate* skip-transition" to "**confirmed** skip-transition:
`liquidate` accepts `Pending`."

### Batch 3 — Authority graph

```json
{ "question": "Which authority roles exist?", "header": "Authorities", "multiSelect": true, "options": [
  { "label": "position_owner — opens / borrows / repays / closes own position", "description": "Per-position signer.",
    "preview": "#[account(mut, has_one = owner)] pub position: Account<'info, Position>, pub owner: Signer<'info>," },
  { "label": "liquidator — any signer can liquidate under-collateralized position", "description": "Permissionless liquidation.",
    "preview": "fn liquidate(ctx: Context<Liquidate>, position_key: Pubkey) -> Result<()> { /* no admin signer; any signer accepted */ }" },
  { "label": "oracle — price feed authority", "description": "Signs price updates consumed by solvency check.",
    "preview": "#[account(constraint = oracle.authority == ORACLE_PUBKEY)] pub oracle: Account<'info, PriceOracle>," },
  { "label": "pool_admin — adjusts collateral_factor / liq_bonus", "description": "Privileged parameter updates.",
    "preview": "fn set_params(ctx: Context<SetParams>, ...) -> Result<()> { /* admin: Signer */ }" }
] }
```

**User clicks:** all four.

**Phase 3 effect:** permissionless-`liquidate` probe is upgraded:
every input that should be rejected (Pending, solvent, already-
Liquidated) becomes a fired-or-silent harness. Producer B queues a
separate oracle-drift sweep and a `compromised oracle` what-if for
Phase 4.

### Batch 4 — Threat scenarios + intentional gaps

```json
[
  { "question": "Which attacker model?", "header": "Threat", "multiSelect": false, "options": [
    { "label": "Unprivileged attacker, no signers", "description": "Anonymous caller against permissionless handlers." },
    { "label": "Compromised user signer", "description": "Attacker holds arbitrary position-owner key." },
    { "label": "Compromised authority (admin / oracle)", "description": "Attacker controls parameter-update or price-feed keys." },
    { "label": "No specific threat model" }
  ] },
  { "question": "Acknowledged shortcuts?", "header": "Shortcuts", "multiSelect": true, "options": [
    { "label": "Oracle staleness window up to 60 slots", "description": "Stale-but-bounded prices accepted by design." },
    { "label": "Admin can drain pool by setting collateral_factor = u64::MAX", "description": "Privileged role trusted." },
    { "label": "Liquidation bonus paid even on zero-debt positions", "description": "Known dust-edge case." },
    { "label": "Interest accrual is per-handler, not per-slot", "description": "Stale interest between calls accepted." }
  ] }
]
```

**User clicks:** attacker = "Unprivileged attacker, no signers";
shortcuts = "Oracle staleness window up to 60 slots" + "Interest
accrual is per-handler, not per-slot."

**Phase 3 effect:** Producer A spends remaining budget on the
permissionless-liquidate surface — the Phase-1 MED was in this class
and the threat-model answer says "find more like that." The stale-
oracle shortcut means the auditor doesn't flag the 60-slot window per
se, but a finding showing the window is **unbounded in practice**
(e.g. no slot-diff check at all) still surfaces — the shortcut
suppresses the intent gap, not the implementation gap.

---

## Structured-answers ratification

In every interactive venue the interview happens in-harness: the agent
renders the probe envelope's `hypotheses[]` (plus its own deeper
candidates) as questions like the transcripts above, collects one
answer per hypothesis (accept / narrow / reject / bug), writes them to
`<audit-dir>/answers.json`
(`{"run_id": …, "answers": [{"id": "<h-… or c-…>",
"decision": "accept|narrow|reject|bug", "note": "…"}]}`), and runs
`qedgen ratify --audit-dir <dir>`. Hypothesis IDs (`h-…`) and legacy
cluster IDs (`c-…`) are addressed uniformly; when `answers.json` is
present, `interview.md` is neither consulted nor required.

**Legacy fallback.** Audit dirs created by older probes still support
the file-driven path: the user edits the dir's `interview.md`, then
`qedgen ratify` parses it. Use it only for those dirs.

Whatever the surface, the same decisions must be asked and the same
ratified data produced. Presentation differences must not alter audit
coverage, evidence, or severity.
