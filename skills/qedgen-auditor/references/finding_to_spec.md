# Finding → spec construct mapping

Operating reference for the audit→specify handoff. Consult this file
when the audit has produced at least one fired MED+ repro and the
agent is about to draft a `.qedspec` that locks the findings in as
permanent regression guards.

The companion document `workflow_walkthrough.md` shows where this
handoff sits in the audit flow. This file is the conversion table:
each probe finding category → the spec construct that prevents the
bug from recurring + the test it produces.

---

## When to consult this file

After the auditor has:

1. Written findings to `.qed/findings/*.md` (one file per finding).
2. Fired at least one MED+ Mollusk / Miri repro (per
   `[[feedback_audit_first_finding_buys_time]]`).
3. Captured per-category data in `.qed/probes/*.json`.

The pitch you carry into the handoff:

> *"I helped find so many bugs, now let's get you to specify them
> so it never happens again."*

For each finding, walk: (a) read the `category` field in the
finding's JSON or markdown, (b) look up the construct family below,
(c) draft the spec snippet, (d) emit it into the user's `.qedspec`
naming the finding it codifies, (e) move to the next finding.

Before converting individual findings, author the specification in three
layers:

1. **Structural:** accounts, identities, PDAs, handlers, authorization, and
   lifecycle shape derivable from code.
2. **Domain:** user-ratified economic equations, units, rounding, temporal
   rules, authority capabilities, and external assumptions from the domain
   dossier.
3. **Regression:** the finding-derived guards in the families below.

Do not claim the protocol is specified merely because every known finding maps
to a guard. Record missing or pending domain properties explicitly.

The agent fills placeholder slots from code-derivable facts
(handler name, field name, error code symbol). Interview the user
**only** for intent-level decisions per
`[[feedback_audit_interview_intent_not_sites]]` — never for facts
the auditor already extracted.

Validate each layer separately. Structural checks establish harness shape;
domain checks define intended behavior; regression checks prove that known bug
shapes fail. Run Crucible again after the domain layer is ratified, even if the
structural skeleton already had a dry fuzz result.

`qedgen ratify` writes `spec-handoff.json` beside the dossier. Treat
`disposition: emitted` as executable only when the generated `.qedspec`
contains the matching `// provenance: domain-candidate <id>` line.
`needs_authoring` means intent is ratified but no executable clause exists.
Use that clause's `authoring.constructs`, parser-shaped `authoring.template`,
and limitations in `authoring.notes` as the starting point; replace every
angle-bracket placeholder from ratified dossier facts and source evidence.
Resolve every `language_gaps[]` entry by extending the language or retaining
an explicit documentary property with a manual lane; never silently count it
as covered. Each gap's `current_language_support` distinguishes missing intent
from a backend limitation and names the nearest executable construct.

Finite aggregate conservation is executable when the binder resolves to
`Fin[N]` (directly or through an alias): use `sum i : Index, ...`, including
inside `old(...)` for pre/post properties. Unbounded sums remain a visible
language gap. `mul_div_floor` and `mul_div_ceil` are executable rounding
constructs. `mul_div_round_half_up` executes nearest rounding with exact halves
rounded upward for non-negative quantities and a positive denominator. Do not
map a generic `nearest` answer to it until the user ratifies that tie policy;
unsupported policies such as ties-to-even remain documentary.
Nominal numeric units are executable with `dimension Lamports = U64` (or
another integer base). Use the dimension name on state fields and handler
parameters; literals adopt the surrounding unit, while incompatible unit
arithmetic, comparisons, and assignments fail during spec checking. Dimensions
erase to their declared integer base at generated-code ABI boundaries.

External assumptions are executable without shadowing program state. Declare
typed scalar fields inside an environment, for example `external clock.slot :
U64`, then constrain post/pre values with `clock.slot >= old(clock.slot)`.
Kani receives distinct nondeterministic `pre_clock_slot` / `post_clock_slot`
values; Lean receives matching theorem parameters. Use the same shape for
oracle values, CPI return data, owner keys, and executable flags. Composite
imported contracts still require explicit scalar projections or a manual lane.

---

## How each entry reads

Every family below has four fields:

| Field | Source |
|---|---|
| **Detection signal** | which probe `Category` variants this family handles + which JSON keys carry the cited slots |
| **Spec construct** | the snippet shape to draft, with `<placeholder>` slots the agent fills from the finding |
| **Why this locks the finding in** | one sentence — gets attached as a doc-comment on the generated spec construct so the user can read the spec later and know which finding it codifies |
| **Failure mode** | what the generated proptest / Kani harness asserts; what goes red on a regression |

---

## Family 1 — Authorization

### Signal

- `Category::MissingSigner` — handler mutates state without a signer
  account in the access set
- `Category::PermissionlessStateWriter` — handler is `permissionless`
  but writes state any caller can grief

JSON keys to read: `handler_name`, `cited_account` (for
`MissingSigner`), `cited_field` (for `PermissionlessStateWriter`).

### Construct

```
handler <handler_name> ... {
  // [from audit finding <id>]:
  //   <handler_name> mutated <cited_field> without verifying the
  //   caller's authority. Locked in by the signer requirement
  //   below.
  requires signer(<cited_account>) else Unauthorized
  ...
}
```

For `PermissionlessStateWriter`: the right answer is one of (a)
remove the `permissionless` marker if it was a mistake, (b) keep
the marker and add a bound on the param the caller controls (see
Family 2), or (c) move the state mutation to a separate authorized
handler. Ask the user which intent applies; don't pick for them.

### Why this locks the finding in

A handler whose signer check is missing today will compile-error or
fail proptest on every regression that tries to mutate the same
field without the signer.

### Failure mode

The proptest harness invokes the handler with a symbolic-signer
account where the signer flag is false. The `requires signer(...)`
guard rejects with the cited error code. Any future patch that
drops the guard makes the proptest fail with "expected
Unauthorized, got Ok".

---

## Family 2 — Numeric guards (bounds + checked arithmetic)

### Signal

- `Category::ArithmeticOverflowWrapping` — `+` / `-` / `*` without
  `checked_*` on a fund-flow path
- `Category::PinocchioUncheckedArith` — same pattern, Pinocchio-side
- `Category::UnboundedAmountParam` — handler param with no
  `requires <= MAX` clause
- `Category::SilentSuccessArithmetic` — `saturating_sub` on a
  timestamp gates a fund-flow comparison (v2.22 S1.1)
- `Category::UncheckedArithWithFundFlow` — unchecked arith touches
  the same handler as a token CPI (v2.22 S1.3)
- `Category::GracefulErrorAsDos` — `checked_sub` whose `Err`
  permanently bricks a PDA (v2.22 S1.2)

JSON keys: `handler_name`, `cited_param` or `cited_field`,
`upstream_bound` (when known), `error_arm_destination` (for
`GracefulErrorAsDos`: the function that handles the Err — `?`
propagation marks "no recovery" intent).

### Construct

**For arithmetic-overflow categories:** swap the spec effect from
`+=` (wrapping by spec semantics) to `+=!` (checked, returns Err
on overflow) or `+=?` (saturating). Pair with a bounded property:

```
handler <handler_name> ... {
  // [from audit finding <id>]:
  //   <handler_name> updated <cited_field> via unchecked <op>.
  //   Switched to checked semantics + invariant bound below.
  requires <cited_param> <= MAX_<UPPER_FIELD> else BoundExceeded
  effect {
    <cited_field> +=! <cited_param>   // checked: was `+=`
  }
}

property <cited_field>_bounded :
  state.<cited_field> <= MAX_<UPPER_FIELD>
  preserved_by all
```

**For `UnboundedAmountParam` alone (no overflow finding):** just
the guard clause; no effect change needed.

**For `SilentSuccessArithmetic` (timestamp saturating_sub):** the
spec lifts the comparison into an explicit branch:

```
handler <handler_name> ... {
  // [from audit finding <id>]:
  //   saturating_sub collapsed "current_ts < <cited_field>" into
  //   "elapsed == 0", opening <effect> when it should have been
  //   blocked. Made the comparison explicit below.
  requires now >= state.<cited_field> else NotYetActive
  ...
}
```

**For `GracefulErrorAsDos`:** the spec adds an else-branch on the
arithmetic that keeps the destination usable. Often the right move
is to bound the param so the underflow is unreachable:

```
handler <handler_name> ... {
  // [from audit finding <id>]:
  //   <cited_field> checked_sub underflow on a PDA-derived address
  //   permanently bricked the PDA. Bounded the input so underflow
  //   is unreachable.
  requires <cited_param> >= state.<source_field> else InvalidInput
  effect {
    <cited_field> := <cited_param> - state.<source_field>
  }
}
```

### Why this locks the finding in

The spec switches from spec-level wrapping semantics to checked
semantics. The generated proptest exercises the overflow path via
`u64::MAX`-shaped inputs and confirms the handler rejects with
the cited error. The bound property covers state-level invariants
that depend on the field staying inside its domain.

### Failure mode

Proptest fuzzes the cited param up to `MAX_VAL`. On the buggy
implementation, the unchecked `+=` overflows and the property
`state.<cited_field> <= MAX_<UPPER_FIELD>` fails post-handler. On
the fix, the `requires` guard rejects before the effect runs.

---

## Family 3 — Lifecycle (state machine)

### Signal

- `Category::LifecycleOneShotViolation` — a "once-only" transition
  (init, finalize, claim) reachable from a state that should be
  terminal
- `Category::InitWithoutPda` — init handler with no PDA constraint;
  default-address collision possible

JSON keys: `handler_name`, `cited_pre_state`, `cited_post_state`,
`expected_terminal` (for one-shot), `cited_account` (for init).

### Construct

**For one-shot violations:** declare the lifecycle states
explicitly and pin the handler's transition:

```
type Lifecycle =
  | Uninitialized
  | Active
  | Finalized

handler <handler_name>
    (...params...)
    : Lifecycle.<cited_pre_state> -> Lifecycle.<cited_post_state> {
  // [from audit finding <id>]:
  //   <handler_name> was reachable from Finalized state, allowing
  //   re-finalization. Pinned the lifecycle to a one-way transition.
  ...
}
```

If the audit found multiple handlers sharing this gap, declare
each handler's transition explicitly. Properties of the form
`state.status != Finalized` `preserved_by all` are NOT what we
want — we want explicit transitions per-handler.

**For `InitWithoutPda`:** the spec's `accounts {}` block adds a
`pda` clause:

```
handler <handler_name> ... {
  accounts {
    <cited_account> : writable, pda [<seed1>, <seed2>]
    // [from audit finding <id>]:
    //   <cited_account> had no PDA derivation — two callers could
    //   target the same default address. PDA pinning below.
    ...
  }
}
```

### Why this locks the finding in

Lifecycle: codegen emits a `status` field with explicit
enum-discriminated transitions. Any handler-call from the wrong
pre-state aborts with `LifecycleViolation`. PDA: codegen emits the
seed derivation and an account constraint that rejects non-PDA
addresses.

### Failure mode

Lifecycle: proptest constructs symbolic states in `Finalized` and
calls the cited handler; assert it rejects. PDA: proptest passes
an arbitrary address as the cited account; assert it rejects unless
the address derives from the cited seeds.

---

## Family 4 — Account constraints

### Signal

- `Category::PinocchioUncheckedAccountLoad` — `borrow_*_unchecked`
  without owner / length / discriminator check
- `Category::PinocchioStaleSafetyComment` — `// SAFETY: …` claims a
  precondition the agent can't find enforced
- `Category::PinocchioAccountTypeConfusion` — same `AccountInfo`
  loaded as T1 in handler A and T2 in handler B
- `Category::ArbitraryCpi` — CPI invocation with attacker-controlled
  target program ID

JSON keys: `handler_name`, `cited_account`, `safety_claim_text`
(for stale-safety), `cited_target_type` (for type-confusion),
`cited_cpi_program` (for arbitrary-CPI).

### Construct

**For `PinocchioStaleSafetyComment`:** lift the SAFETY claim into a
spec invariant:

```
// [from audit finding <id>]:
//   The SAFETY comment at <handler_name> claimed "<safety_claim_text>"
//   but the auditor could not find this enforced on every CF path.
//   Lifted as an explicit invariant so the harness checks it.
invariant <slug_of_safety_claim>
  "<safety_claim_text — pasted verbatim>"
  -- or, if expressible: state.<cited_field> ... else <ErrorName>
```

If the SAFETY claim is expressible as a `requires` clause (length,
discriminator, owner check), promote it directly into the
`accounts {}` block.

**For `PinocchioUncheckedAccountLoad`:** the spec's `accounts {}`
block declares the expected discriminator / owner:

```
accounts {
  <cited_account> : writable, owner <program_id>, discriminator <DISC>
}
```

**For `PinocchioAccountTypeConfusion`:** use distinct typed account
references in the spec; the agent walks the program to pick the
right type name per handler.

**For `ArbitraryCpi`:** spec lockdown via the `call` clause:

```
call <ExpectedProgram>.<method>(...args...)
// [from audit finding <id>]:
//   <cited_cpi_program> was a parameter; an attacker could swap it
//   for a fake program. Pinned the call target above.
```

### Why this locks the finding in

Type/owner/discriminator constraints live in the spec's account
shape; codegen emits the checks at the entry of every handler that
touches the account. CPI target is bound at spec-time; codegen
emits the constant program ID into the CPI builder.

### Failure mode

Proptest passes accounts with wrong owners / wrong discriminators
/ wrong types and asserts the handler rejects. CPI: codegen
hardcodes the program ID; runtime CPI dispatcher rejects mismatched
targets.

---

## Family 5 — Cross-handler consistency

### Signal

- `Category::PairedValidatorInputDomainMismatch` — two handler-level
  validators accept different domains for the same logical field
  (v2.22 S2.1)

JSON keys: `handler_a_name`, `handler_b_name`, `cited_field`,
`domain_a` (accept-domain in handler A), `domain_b` (accept-domain
in handler B).

### Construct

Extract the validation to a single named guard shared across both
handlers:

```
guard <cited_field>_valid : state.<cited_field> ... else <ErrorName>
// [from audit finding <id>]:
//   handler_a accepted <domain_a>; handler_b accepted <domain_b>.
//   Extracted the canonical accept-domain below; both handlers
//   reuse it via `requires`.

handler <handler_a_name> ... {
  requires <cited_field>_valid
  ...
}

handler <handler_b_name> ... {
  requires <cited_field>_valid
  ...
}
```

**Intent decision the user must make:** which domain is the
canonical one? The agent surfaces both `domain_a` and `domain_b`
and asks: "the audit found these two distinct accept-domains for
`<cited_field>` — which is correct?" Then drafts the spec with
that domain.

### Why this locks the finding in

The spec has one source of truth for the field's valid domain. A
future change that loosens one handler's check without loosening
the other surfaces as a `requires` mismatch in codegen review.

### Failure mode

Proptest generates inputs that satisfy `domain_a` but violate
`domain_b` (or vice versa) and asserts both handlers reach the same
verdict.

---

## Family 6 — Resource teardown

### Signal

- `Category::ExternalAuthorityNotRevokedOnClose` — close handler
  destroys a PDA that holds delegate / mint authority on an external
  account, without issuing the reverse CPI (v2.22 S4.1)

JSON keys: `handler_name`, `cited_pda`, `cited_external_account`,
`authority_kind` (delegate / mint / freeze / ATA delegate).

### Construct

```
handler <handler_name> ... {
  // [from audit finding <id>]:
  //   <cited_pda> held <authority_kind> on <cited_external_account>
  //   but close() didn't revoke. Added revoke CPI below.

  ...existing close logic...

  call Token.revoke(authority <cited_external_account>)
    // or SetAuthority::None / Assign — pick per authority_kind
}
```

The agent picks the right CPI shape from `authority_kind`:
- `delegate` → `Token.revoke`
- `mint` / `freeze` → `Token.set_authority(None)`
- `ATA delegate` → `Token.revoke` on the ATA

### Why this locks the finding in

The spec's `call` clause encodes the teardown obligation as part
of the close handler's contract. Codegen emits the CPI in the
generated handler skeleton; harness asserts the external account's
authority field is null post-handler.

### Failure mode

Proptest closes the PDA, then asserts that
`<cited_external_account>.authority == None` (or the equivalent
sentinel). The buggy implementation leaves the field populated.

---

## Family 7 — Spec-refinement (no new construct)

Some categories don't add a spec construct — they refine an existing
one or surface a structural issue the user must resolve at code level.

### Signal

- `Category::StoredFieldNeverWritten` — state field declared but no
  handler writes it. Two intents possible: (a) the field is
  documentation that needs an effect, (b) the field is dead code
  that should be removed.
- `Category::CrucibleFuzzCrash` — Crucible found a sequence that
  violates an existing property. The spec is correct; the fix is
  either tighten `preserved_by`, add a new sub-property, or fix the
  implementation.

### Action

For `StoredFieldNeverWritten`: ask the user "is `<cited_field>`
intended to be written by any handler? If yes, which one?" Then
add the effect to the cited handler. If no, drop the field from
the spec (and ideally the impl).

For `CrucibleFuzzCrash`: the failing trace is in
`.qed/probes/crucible/<finding-id>/trace.json`. Convert the trace
into a unit test fixture; either (a) tighten the property's
`preserved_by` to exclude the trace's handler chain (rare — usually
indicates the property was over-claimed), or (b) introduce a new
property the failing trace specifically violates, with
`preserved_by` covering the handlers in the trace.

---

## Family 8 — Out of band (documentation only)

Some categories don't lend themselves to a spec construct in v2.23:

- `Category::ExecutionDivergence` (Miri-fail / Mollusk-pass) — the
  fix is implementation-side (use `*mut T` correctly, satisfy
  alignment, etc.). The spec can carry a doc invariant naming the
  divergence but the verification is via Miri, not proptest.
- `Category::PinocchioMutableBorrowAliasing` — implementation-side
  fix (drop overlapping borrows).
- `Category::PinocchioPositionWithoutTypeTag` — see Family 4.
- `Category::PinocchioOffsetOverrun` — bound the offset upstream;
  spec captures the bound but the fix is in the impl.
- `Category::PinocchioMissingPdaVerification` — see Family 3
  (InitWithoutPda equivalent).

For these, emit a documentation invariant:

```
invariant <slug_of_finding>
  "<finding summary>. See .qed/findings/<id>.md for the
   reproducer. Verification lives at the implementation level."
```

The doc invariant surfaces the finding in reports and keeps the
narrative coherent (every finding has a spec-side trace), even
though the harness doesn't directly check it.

---

## Worked example

Audit on a brownfield Pinocchio program produces three findings:

```
.qed/findings/001-arithmetic_overflow_wrapping.md
.qed/findings/002-missing_signer.md
.qed/findings/003-pinocchio_stale_safety_comment.md
```

Walking each:

### Finding 001 — ArithmeticOverflowWrapping

JSON cites: `handler_name = deposit`, `cited_field = balance`,
`cited_param = amount`, `upstream_bound = none`.

Family 2. Draft:

```
handler deposit (amount : U64) : Lifecycle.Active -> Lifecycle.Active {
  // [from audit finding 001]:
  //   deposit updated balance via unchecked +; switched to checked.
  requires amount <= MAX_BALANCE else BoundExceeded
  effect {
    balance +=! amount
  }
}

property balance_bounded :
  state.balance <= MAX_BALANCE
  preserved_by all
```

### Finding 002 — MissingSigner

JSON cites: `handler_name = withdraw`, `cited_account = owner`.

Family 1. Draft:

```
handler withdraw (amount : U64) : Lifecycle.Active -> Lifecycle.Active {
  // [from audit finding 002]:
  //   withdraw mutated balance without verifying owner signed.
  requires signer(owner) else Unauthorized
  ...
}
```

### Finding 003 — PinocchioStaleSafetyComment

JSON cites: `handler_name = init`,
`safety_claim_text = "config_pda owner verified by caller"`,
`cited_account = config_pda`.

Family 4. Promote the SAFETY claim to a constraint:

```
handler init ... {
  accounts {
    config_pda : writable, owner program_id
    // [from audit finding 003]:
    //   SAFETY comment claimed "config_pda owner verified by
    //   caller" but the verification was unreachable. Promoted
    //   to an explicit owner constraint above.
    ...
  }
}
```

After all three findings convert, run `qedgen check` → iterate to
lint-clean → `qedgen codegen --all` → `qedgen verify --proptest`.
Expect: 3 new proptest failures against the buggy code (one per
finding); 3 green after the obvious fixes.

That's the audit→specify loop.
