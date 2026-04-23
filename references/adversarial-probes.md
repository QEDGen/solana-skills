# Adversarial Probes Reference

This is a checklist Claude walks manually during §3a.5 — the attack-surface
scan that happens after the initial `.qedspec` is drafted but before lint
or counterexample tiers. There is no `qedgen probe` CLI command (that's
v2.7 scope); the taxonomy below is applied by the in-session agent against
the spec + source. Users rarely enumerate these classes on their own; the
probes do it for them.

## How Claude uses this file

For each probe below, Claude asks the question against the drafted spec
+ source. If the answer is "yes, this surface is exposed and
unprotected," Claude:

1. Adds the corresponding negative-path obligation to the spec —
   typically `aborts_if` or a property — so proptest/Kani have a
   concrete target.
2. Records the hit in `.qed/plan/findings/NNN-<probe-slug>.md` with the
   pattern shape, not the incident details.
3. Surfaces **only the findings** to the user: "I noticed X could Y;
   I've added an `aborts_if` to defend. Confirm or adjust." The probe
   list itself stays out of the conversation.

Probes that don't hit leave no trace. Silent when sound, loud when not.

---

## Class A — Arithmetic & casting

### A1. Narrowing cast from generic or const parameter

**Probe:** Is there an `as uN` (or `as iN`) cast whose source value
derives from a generic type parameter `T` or a const generic parameter
`N`, without a force-evaluated compile-time bound assertion that `N`
(or `size_of::<T>()`) fits within the destination width?

**Why it matters:** associated const assertions are lazy — they don't
fire at monomorphization unless referenced. A `const _: () = assert!(N
<= u16::MAX)` that nothing references will silently compile, and the
`as u16` cast will silently wrap at the `(u16::MAX + 1)`-th value.

**Spec obligation when it fires:**
```
property no_narrowing_wrap "writes never truncate"
  preserved_by: <handler_using_cast>

handler <h> {
  aborts_if <source_expr> > U16_MAX with NarrowingWrap
}
```

### A2. Unchecked `mul` / `add` / `sub` on attacker-influenced operands

**Probe:** Does any handler perform `mul` / `add` / `sub` (or `+` / `*`
/ `-`) without a checked variant or a prior guard, when at least one
operand is derived from instruction data or an account field the caller
can set?

**Why it matters:** release builds wrap on overflow rather than panic.
Silent wrap-around is a state-corruption vector — an attacker sets the
operand to force the wrap and leaves the program in a state the invariants
don't cover.

**Spec obligation when it fires:** either `aborts_if <operand> > BOUND`
before the arithmetic, or a conservation/bounds property preserved by
the handler (which will flag the overflow as a proptest/Kani hit).

### A3. `usize` subtraction with external-mutation reachability

**Probe:** Is there a `usize - usize` (or equivalent unsigned
subtraction) where the RHS can exceed the LHS after some external
mutation — e.g. `capacity - len` after a concurrent `shrink`, or
`account.data.len() - header_len` after a realloc shrink?

**Why it matters:** unsigned subtraction underflow in release builds
wraps to a huge value. If that value feeds into a slice index or bound,
the next access can read far past the intended buffer.

**Spec obligation when it fires:** assert the subtraction ordering as a
`requires` on the handler, or add an `aborts_if` for the underflow
condition explicitly.

---

## Class B — State machine & close-safety

### B1. Handler reachable in a state it wasn't designed for

**Probe:** For each handler, is the `when: <state>` set tight enough to
prevent execution in states where the handler's effects violate an
invariant? Common misses: a `withdraw` handler reachable in
`Uninitialized`, a `finalize` handler reachable in `Closed`.

**Why it matters:** replay and re-init attacks depend on a handler being
callable in a state the author didn't consider. Tight `when`/`then` sets
turn "impossible" into "cannot type-check."

**Spec obligation when it fires:** narrow the `when:` clause; add a
`lifecycle` declaration if none exists.

### B2. Close handler that zeroes owner/lamports but leaves the discriminator

**Probe:** Is there a close handler (any handler that sets
`account.data_len = 0` or transfers lamports out such that the account
becomes rent-exempt-failing) that does **not** also overwrite the
account's discriminator / first 8 bytes with a sentinel?

**Why it matters:** if the runtime doesn't zero closed-account data in
the same transaction, a re-init racing the close can read stale fields
(authority, admin keys) from the old state. The classic defense is
writing a `CLOSED_ACCOUNT_DISCRIMINATOR` sentinel before closing.

**Spec obligation when it fires:**
```
handler close {
  ensures post(account.data[..8]) == CLOSED_SENTINEL
}
```

### B3. Read after external resize

**Probe:** Does any handler read `account.data[i]` for `i > 0` after a
path where the account could have been reallocated smaller by a prior
instruction? Slab-based / cursor-based account layouts are the usual
offenders.

**Why it matters:** a shrunken account's `data.len()` is smaller, but
header fields can still hold pre-shrink offsets. Reads via those
offsets panic or underflow.

**Spec obligation when it fires:** `requires account.data.len() >=
<offset>` on the handler, or a lifecycle invariant forbidding reads
after a `resize` variant.

---

## Class C — Borrow / aliasing protocol

### C1. Release-then-reacquire losing writes

**Probe:** Does any handler take a mutable borrow on an account, write
to it, release the borrow (explicit `drop`, end of scope, or pass
through a function that internally re-borrows), and then take a **new**
borrow on the same account without re-reading the updated state?

**Why it matters:** if the reacquire path constructs a fresh view from
the underlying account buffer rather than observing the prior write,
the write is effectively lost — but the handler returns `Ok` and the
caller thinks the update landed.

**Spec obligation when it fires:** restructure the handler to hold one
borrow for the full write window; add a `property
write_observed_by_next_read` the handler must preserve.

### C2. Borrow-state byte protocol assumptions

**Probe:** Does the handler assume the account's borrow-state byte is
`NON_DUP_MARKER` (i.e. this account is not a duplicate of an earlier
entry in the instruction's account list)? Does the loader actually
enforce that, given the caller's duplicate-detection rules?

**Why it matters:** duplicate-writable accounts in a single instruction
violate Rust's aliasing rules — the framework is supposed to reject
them, but programs that reach past the framework to raw account views
can observe duplicates and crash with UB.

**Spec obligation when it fires:** `requires account.borrow_state ==
NON_DUP_MARKER` (or equivalent predicate), making the assumption
explicit for proptest/Kani to exercise.

---

## Class D — Authorization

### D1. Missing signer check on authority-derived field update

**Probe:** For every handler that mutates a field named `authority`,
`owner`, `admin`, `delegate`, or anything structurally equivalent (a
`Pubkey` field granting privilege), does the handler require the
**current** authority to sign?

**Why it matters:** a missing signer check on authority rotation is an
instant take-over — anyone can call the handler and set themselves as
authority.

**Spec obligation when it fires:** `who: <current_authority_ref>` on the
handler, or `requires signers contains state.authority`.

### D2. Missing owner check on typed account

**Probe:** For every account the handler treats as a typed struct
(deserialize, field access), does the spec's context block or the
handler body verify `account.owner == PROGRAM_ID`?

**Why it matters:** without the owner check, an attacker can pass a
look-alike account owned by a different program whose byte layout
happens to match. The handler reads arbitrary attacker-controlled data.

**Spec obligation when it fires:** annotate the account in the
`context:` block with the expected owner, or add
`requires account.owner == PROGRAM_ID`.

### D3. PDA seed uniqueness under concurrent init

**Probe:** For every `init`-style handler creating a PDA, is the seed
set unique per principal (e.g. includes the authority pubkey or a
user-specific nonce)? Or can two different callers derive the same PDA
and race to initialize it?

**Why it matters:** colliding PDAs mean the second caller's init
transaction fails, but only after the first caller has locked in their
ownership. Worse, if the seed includes only a program-wide counter, an
attacker can front-run the intended initializer.

**Spec obligation when it fires:** extend the PDA seed list in the
`context:` block with the distinguishing principal; add a `cover` to
prove the intended initializer path is still reachable.

---

## Class E — Conservation

### E1. Sum-across-accounts invariant with partial update

**Probe:** Is there a declared or implicit conservation law of the form
`state.F == sum(account_i.G)` where multiple handlers mutate individual
`G` fields? For each such handler, does it also update `F` (or is
mutation of `G` forbidden in its `effect:` block)?

**Why it matters:** a handler that updates a single account's balance
without updating the protocol-wide total breaks solvency silently. The
bug only surfaces when someone queries the total and finds it
inconsistent — typically much later.

**Spec obligation when it fires:** add the conservation law as an
explicit `property`, `preserved_by:` each handler that touches any
summand. Proptest/Kani will then find any handler that violates it.

### E2. Token-transfer balance across CPI

**Probe:** For every `call Token.transfer(...)` or equivalent CPI moving
tokens, does the spec tie the amount to a field the caller can't trivially
desynchronize from the post-state? E.g. `amount = old(state.vault) -
state.vault` or `amount = state.outgoing`.

**Why it matters:** if the spec just says `call Token.transfer(amount =
user_input)`, the handler can transfer any amount and the state machine
won't care. Conservation is preserved by external tie-back, not by the
call itself.

**Spec obligation when it fires:** bind `amount` in the `call` clause
to a spec-derived expression, plus an `ensures` relating pre- and
post-state balances.

---

## Class F — CPI execution

### F1. Account ordering assumed stable across `sol_invoke_signed`

**Probe:** Does the handler read from an account reference *after* a
CPI returns, assuming the account list order was preserved? Does the
spec declare the CPI's `preserves_account_order` contract?

**Why it matters:** most CPIs do preserve order, but the protocol
doesn't guarantee it in general — and if the callee is itself a
qedgen-verified program, its spec might permute. Without a declared
contract, Kani has nothing to check against.

**Spec obligation when it fires:** add an `ensures
preserves_account_order` on the Tier-1 interface for the callee.

### F2. Realloc lamport-transfer disjointness

**Probe:** Does a handler call `realloc` on an account and also move
lamports into or out of that same account in a way that could alias the
realloc's data-buffer borrow?

**Why it matters:** realloc internally takes a mutable view of the
account's data buffer. A concurrent lamport transfer that borrows the
account (even read-only) can violate aliasing invariants. In practice
the framework splits the borrows, but hand-rolled CPI sequences have
been known to miss this.

**Spec obligation when it fires:** sequence the realloc and transfer
explicitly in the handler body; add a `property
no_concurrent_account_borrow` for the handler.

### F3. Data-increase bound not enforced

**Probe:** Does the handler perform a realloc whose new size can exceed
`MAX_PERMITTED_DATA_INCREASE` (10 KiB per transaction at the time of
this writing)?

**Why it matters:** runtime enforces the limit; violating it aborts the
transaction. If the handler doesn't guard upfront, the error surfaces
to the end user as "transaction failed" with no helpful diagnostics,
and any partial state changes before the realloc are rolled back —
including side effects the caller depended on.

**Spec obligation when it fires:** `aborts_if new_size - old_size >
MAX_PERMITTED_DATA_INCREASE` with a named error.

---

## Extending this taxonomy

New probes land here when the telemetry corpus (§SKILL.md "Learning
capture") shows a pattern firing across multiple projects. Criteria for
promotion from `.qed/plan/findings/` to a formal probe:

1. **Three independent hits** across unrelated codebases.
2. **Generalizable shape** — can be expressed as a syntactic or
   semantic question answerable from spec + source alone.
3. **Concrete spec obligation** — there's a clear `aborts_if` /
   `property` / structural change to add when the probe fires.

Probes that don't meet all three stay in `findings/` as observations
until they do.
