# Intrinsic candidates vs. real fixtures

**Captured:** 2026-05-24, `mir` branch off v2.29.2.

**Purpose.** Walk every handler in `examples/` and tag which feature it uses, mapping to a candidate MIR node from issue #66's intrinsic list. The bug-reduction framing per [[feedback-mir-is-bug-reduction]] says: a node belongs in MIR if it eliminates a class of cross-codegen divergence bug (see `codegen-divergence.md`), grounded in real handlers — not if it's a theoretically pretty fit. This inventory is the reality check.

**Corpus.** 19 non-regression fixtures across `crates/qedgen/tests/fixtures/anchor-brownfield-demo/`, `examples/rust/{cross-program-vault,multisig,lending,brownfield-onboarding,escrow,escrow-split,bundled-stdlib-demo}/` (the perp-dex risk-engine example was in the original 20-fixture sweep; it has since been removed), `examples/sbpf/{tree,slippage,transfer,counter}/`, `examples/rust/cross-program-vault/imports/cross-program-vault-admin/`. Plus the 28 regression fixtures consulted as supplementary evidence. (The sBPF order-book example is temporarily out pending a proof re-port to the qedsvm region-table semantics; its codegen fixture remains under `crates/qedgen/tests/fixtures/`.)

## Feature frequency table

| Feature | Files (of 21) | Total uses | Spec syntax | Candidate MIR shape |
|---|---:|---:|---|---|
| Lifecycle transition | **16** | 56 | `: State.V1 -> State.V2` in handler signature | `HandlerMir.transition: Option<(VariantTag, VariantTag)>` (not a Stmt) |
| `init` annotation in accounts | 16 | 100 | `account_name : writable, init, …` | `AccountBinding.init: bool` (not a Stmt; Anchor lowers as `#[account(init)]`) |
| Abort branches | **15** | 96 | `requires X else ErrName` | `Stmt::Branch(pred, ok_block, Stmt::Abort(err_ref))` |
| `writable` annotation | 14 | 114 | `account_name : writable, …` | `AccountBinding.writable: bool` |
| Signer check | **14** | 47 | `auth <account_or_field>` | `Stmt::SignerCheck { account }` or `HandlerMir.auth` |
| Pda declaration | 11 | 37 | top-level `pda <name> [seeds]` | `Mir.accounts: AccountTable` entry; no Stmt needed |
| `authority` annotation | 9 | 74 | `account_name : type token, authority <other>` | `AccountBinding.authority: Option<AccountRef>` |
| Pda inline in account | 9 | 27 | `acct : writable, pda [seeds]` | same as above (declaration-style); `Stmt::Pda` not needed |
| Effect assign (`:=`) | 10 | 18 | `field := value` inside `effect { … }` | `Stmt::Assign(path, expr)` |
| `call Token.transfer` | 5 | 8 | `call Token.transfer(from, to, amount, authority)` | `Stmt::TokenTransfer { from, to, amount, authority }` |
| `transfers { … }` block | 2 | 7 | `transfers { from A to B amount X authority Z }` | `Stmt::TokenTransfer` (same) |
| Events (`emits`) | 6 | 17 | `emits EventName` | Out of MIR scope — auxiliary handler-level metadata |
| `type token` (token account) | 6 | 24 | `account_name : type token, …` | `AccountBinding.kind = Token` |
| `readonly` annotation | 5 | 7 | `account_name : readonly, …` | `AccountBinding.writable = false` |
| Quantifier (`forall`) | 3 | 6 | `forall a in coll, P(a)` | Opaque expression carrier (per MIR opaque-expr constraint) |
| Wrap arithmetic (`+=!`) | 2 | 2 | `field +=! delta` | `Stmt::WrapAdd(path, expr)` |
| Variant promotion (`state := .V`) | **1** | 1 | `state := .Closed { … }` | `Stmt::VariantPromote { from_tag, to_tag, payload }` |
| `exists` | 1 | 1 | `exists s in signers, …` | Opaque expression carrier |

## What's in the proposal's intrinsic list but **not** in the fixtures

The MIR proposal (#66) lists 15 candidate intrinsic node kinds. Comparing each to the fixture evidence:

| Proposal node | Fixture evidence | Verdict |
|---|---|---|
| `Pda` (statement-level) | Zero. PDAs are declared top-level (`pda <name> [seeds]`) and resolved into account bindings, never appear inline in handler bodies. | **Demote.** Carry as a `Mir.accounts` entry shape, not a `Stmt`. |
| `Cpi` (generic) | One occurrence of `call Target.h` in an interface-test regression fixture. Everything else is `Token.transfer`. | **Keep as escape hatch only.** Don't try to model generic CPI as a primary intrinsic until a real generic use case shows up. |
| `SystemTransfer` | Zero. Lamport movement happens through Anchor's `#[account(init, ...)]` declaratively. | **Defer.** No fixture evidence; speculative. |
| `AccountInit` | Implicit in `init` annotations (16 files), but never appears as a handler-body statement. | **Demote.** Account-block lowering concern, not `Stmt`. |
| `AccountClose` | Zero. | **Defer.** |
| `TokenTransfer` | 7 fixtures, 15 total uses across `transfers` blocks + `call Token.transfer`. | **Keep.** Strongest signal. |
| `TokenMint` | Zero. | **Defer.** |
| `TokenBurn` | Zero. | **Defer.** |
| `TokenApprove` | Zero. | **Defer.** |
| `SysvarRead` | Zero. | **Defer.** |
| `DiscriminantMatch` | Zero in `.qedspec` (handlers carry their own discriminant declaratively, not as a body statement). | **Demote.** Handler-level metadata, not `Stmt`. |
| `SignerCheck` | 14 files. Declarative (`auth <field>`) rather than inline. | **Demote to `HandlerMir.auth`.** Not a body statement in current specs. |
| `LamportAssert` | Zero. | **Defer.** |
| `TokenExt` | Zero. | **Defer.** |
| `ProgramSpecific` | Zero. | **Defer.** |

## What's in the fixtures but **not** in the proposal

- **Lifecycle transitions** — present in 16/21 fixtures, but issue #66 puts them in `HandlerMir.transition` as metadata, not a `Stmt`. The fixture evidence validates that — lifecycle is a handler-level field, not body-level.
- **Abort branches via `requires X else Err`** — present in 15/21 fixtures. Proposal models as `Stmt::Branch(pred, ok, Stmt::Abort(err))`. Fixture evidence supports the modelling but a more direct `Stmt::RequireOrAbort(pred, err)` shape would be cleaner — it's the canonical pattern, and lowering would be uniform across codegens.

## Recommended initial intrinsic set (fixture-driven)

Based on the bug-reduction framing — only nodes that close a divergence class **and** appear in real fixtures:

**Primary intrinsics** (clear divergence-class closer, ≥5 fixtures):

1. **`Stmt::TokenTransfer { from, to, amount, authority }`** — 7 fixtures. Closes A2-shape divergence (Kani + proptest currently can't tell what a CPI does to caller state). Plus A4 (CPI ensures lowering coordination). Validated as primary.

2. **`Stmt::RequireOrAbort(pred, err_ref)`** — 15 fixtures, 96 uses. Closes A3 (abort-semantics divergence). The most-used non-arithmetic feature. Probably worth a dedicated node rather than reducing to `Stmt::Branch(.., Stmt::Abort(..))`, because the canonical `requires X else Err` shape is overwhelming.

**Secondary intrinsics** (close a divergence class, narrower fixture evidence):

3. **`Stmt::Assign(path, expr)`** — 10 fixtures, 18 uses. The escape-hatch for effect bodies. Closes B1 (effect-op dispatch duplication).

4. **`Stmt::CheckedAdd(path, expr, err_ref)` / `CheckedSub` / `WrapAdd` / `SatAdd`** — 2 fixtures with `+=!` markers, broader use latent in declared effects. Per-effect-error variants per [[project-v224-per-effect-errors]]. Closes B1.

5. **`Stmt::VariantPromote { from_tag, to_tag, payload }`** — 1 main fixture + several regressions. Closes A2 (Kani/proptest variant-promotion gap). Worth including despite low fixture count because the divergence is severe.

6. **`Stmt::Branch(pred, then_block, else_block)`** — needed for issue #42 conditional effects. Closes A1 (ParsedEffectBranches divergence). Low fixture count in main corpus but the entire regressions/issue-42-conditional/ tree exists for it.

**Demoted to handler-level metadata** (not statements):

- `HandlerMir.transition: Option<(VariantTag, VariantTag)>` — 16 fixtures
- `HandlerMir.auth: Option<AccountOrField>` — 14 fixtures
- `HandlerMir.discriminant: Option<u8>` — fixture-implicit
- `Mir.accounts: AccountTable` — carries `writable`, `init`, `authority`, `pda` declarations + token-account markers

**Generic escape hatches** (catch-all for non-intrinsic handlers):

- `Stmt::Let(symbol, expr)`
- `Stmt::Branch(pred, then, else)` (also serves as a Stmt::RequireOrAbort generalization if needed)
- `Stmt::Abort(err_ref)`
- `Stmt::Cpi { target, method, args }` (escape hatch for the one `call Target.h` regression case and any future non-Token CPIs)

**Total initial set:** ~10 statement kinds (6 primary/secondary intrinsics + 4 escape hatches) + ~3 handler-level metadata fields + 1 top-level `AccountTable`.

This is **half** the proposal's 15-statement list. Everything dropped (SystemTransfer, AccountInit, AccountClose, TokenMint, TokenBurn, TokenApprove, SysvarRead, DiscriminantMatch, SignerCheck-as-Stmt, LamportAssert, TokenExt, ProgramSpecific) is deferred until a real fixture demands it.

## Implications for #66

1. **The "≥3 of 5 codegens" rule was the wrong gate.** The right gate is fixture-evidence + divergence-class closure. Half the proposal's list fails the fixture-evidence test.

2. **Account-block features (writable, init, authority, type, pda) account for huge LoC across codegens** (114 + 100 + 74 + 24 + 27 = 339 total references across fixtures). The `AccountTable` design is where the most cross-codegen leverage sits — bigger than any single `Stmt` kind. Phase 0 should probably start with `AccountTable` design before `Stmt`.

3. **Lifecycle is metadata, not flow control.** 16 fixtures use lifecycle transitions, but the spec syntax (`: State.V1 -> State.V2`) is a handler-level declaration, never a body statement. MIR design should reflect this — don't emit `Stmt::Transition`; carry it on `HandlerMir`.

4. **`Stmt::RequireOrAbort` is the most-used node by raw count** (96 uses in 15 fixtures). Probably worth a dedicated shape rather than synthesizing from `Branch + Abort`. The lowering is uniform across codegens (proof: each abort gets a hypothesis; Anchor: early-return; Kani: assume; proptest: prop_assume) — so it's a clean abstraction barrier.

5. **TokenTransfer dominates the CPI surface.** Of 9 total CPI occurrences across all 49 fixtures (main + regressions), 8 are `Token.transfer` and 1 is a synthetic interface test. The proposal's generic `Stmt::Cpi` is reachable but should be a *secondary* node behind the canonical `Stmt::TokenTransfer`. Don't promote SystemTransfer / TokenMint / TokenBurn / etc. into primaries until specs use them.

6. **Phase 1 pilot can stay TokenTransfer-anchored.** The fixture evidence confirms it's the right pilot — multiple fixtures, clean divergence-class closure (A2 + A4), and the lowering is well-understood per codegen.
