# QEDGen v2.42.0 — the eight-pass auditor

**Status:** shipped (PRs #207, #209, #210, #211, #212, #213, #214).
**Theme:** an auditor overhaul focused on **recall, precision, and
consistency** — three new cross-cutting lenses, a prescriptive severity
procedure, an adaptive multi-run loop, and a data-grounded catalog cleanup —
plus an IDL-fidelity fix. No breaking changes; no DSL changes (those stay
reserved for v3.0).

Every auditor change below was validated against real, independently-audited
programs (a benchmark corpus of firm-audited Solana protocols across escrow,
subscription, multisig, vault, lending, and governance domains): each new lens
is backed by a concrete finding it recovers that the prior passes walked past.

## What shipped

### 1. Three new cross-cutting passes — §3d, §3e, §3f (#207, #210)

The auditor's "Investigate" step now runs **eight** cross-cutting passes
(§3a–§3h) alongside the per-category catalog. Three are net-new coverage —
classes the catalog never had:

- **§3d — comparison-direction / inverted-guard sweep.** A guard that is
  present and plausible-looking but enforces the *opposite* of its intent
  (`<` where `>` was meant, an inverted accumulation sign). The arithmetic
  probe keys off the operator *symbol*, not direction, so this class slips it;
  §3d is a read discipline (direction-correctness needs intent), falsifiable
  via a `.qedspec` Kani/proptest property when a spec exists.
- **§3e — store-without-validate sweep.** A handler that persists an external
  account/`Pubkey` into state with no on-curve / owned / signer /
  PDA-derivation check, so a later handler trusts a value never checked at
  write time. Distinguished from §3b (role anchoring in *this* handler).
- **§3f — dead-guard / unwired-error-variant sweep.** An error variant defined
  in `errors.rs` but wired into no guard — a named-but-never-enforced check.
  Enumerate the enum, grep each variant for an enforcement call-site, flag the
  zero-call-site ones. A dead guard inherits the impact ceiling of the path it
  fails to protect (see the severity procedure below).

### 2. Two more passes — §3g, §3h (#210)

- **§3g — state-machine / lifecycle-transition soundness sweep.** Two shapes:
  a premature transition (a container advanced to active/finalized before all
  its parts are added, then locked), and a bricked permissionless create/init
  (revert-DoS when the target address is pre-funded above rent-exempt).
- **§3h — zero / sentinel-value asymmetry sweep.** A sentinel (`0` / `u64::MAX`
  / `Pubkey::default()` / empty) one handler rejects while another honors as
  meaningful, plus a one-sided-bound check (a window guarded on only one end).

### 3. Severity as a prescriptive decision-procedure + provenance tag (#211)

Severity guidance, previously scattered, is now **one four-step procedure**
applied to every finding — (1) rate the impact ceiling assuming the
precondition holds, (2) record the gate as a qualifier not a discount, (3)
downgrade only when capability is absent or unreachable, (4) special cases
(LOW-composes-to-CRIT; dead-guard inherits the unguarded path's ceiling). A
prescriptive procedure is applied more consistently than scattered prose. Each
finding now also carries a mandatory **`Surfaced by:`** tag (the pass /
category / probe that found it) — a standing fire-rate signal.

### 4. Passes are primary; catalog is evidence (#212, #214)

A cross-reference table establishes the eight passes as the primary read-driven
surface and maps the catalog categories a pass owns, killing the
pass↔catalog drift. And the four **qedgen-codegen-only** categories
(`generated_guard_bypass`, `stored_field_never_written`,
`spec_impl_drift_user_owned`, `qed_hash_drift_or_forgery`) now carry an explicit
brownfield-skip callout — a fire-rate analysis across a domain-diverse corpus
confirmed they are the *only* categories that never fire on a hand-written
audit target (they apply solely to generated code), so a brownfield audit skips
them: less always-scanned catalog effort, zero recall loss.

### 5. Adaptive N-run union (#213)

The high-stakes N-run union is now an **outcome-driven loop**, not a fixed
batch: surface a MED+ finding the instant a run produces it; a find is not a
stop signal — keep running behind the scenes (runs routinely catch *different*
top findings); a dry run is under-sampling evidence, not an all-clear, so run
again; stop on convergence (K consecutive dry runs), budget, or user. N is a
floor, not a ceiling.

### 6. IDL fidelity — enum `definedTypes` + a test-stack fix (#209, #202)

- Codama/Anchor enum `definedTypes` now render as real DSL sum types
  (`type AccountState | Uninitialized | Initialized | Frozen`) instead of a
  generic `Uninitialized | Active` lifecycle. `IdlTypeBody` gains a `variants`
  carrier; the Codama normalizer synthesizes it from an `enumTypeNode`.
- A deeply-nested brownfield-Kani test now runs on a large-stack thread —
  clears a debug-build stack-overflow that aborted `cargo test` on macOS
  (release and CI were unaffected; the generator was always correct).

## Compatibility

No breaking changes. Generated codegen output is unchanged except the
version-tag re-stamp in bundled example `Cargo.toml` pins. The auditor changes
are all in the skill surface (`skills/qedgen-auditor/`); the CLI/codegen change
is the IDL enum fix.
