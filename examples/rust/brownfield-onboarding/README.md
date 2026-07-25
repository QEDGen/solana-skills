# Brownfield onboarding walkthrough

This example demonstrates QEDGen's brownfield first-contact flow:
*audit a Solana program → convert findings into a `.qedspec` →
verify the harness fires red on the buggy obligation and green on
the corrected one.* The shipped code and spec are the FIXED variant;
the bug is documented inline as one-line diffs you apply to
reproduce the audit's red-then-green cycle.

The example's value proposition is the **red-then-green sequence**,
not "tests stay green." Any drift gate running against this
directory should treat that as an intentional asymmetry — the
committed source is the fixed final state of the cycle.

## Layout

```
brownfield-onboarding/
├── README.md                                 # this file — the walkthrough
├── onboarding.qedspec                        # the spec scaffold (FIXED)
├── qed.toml                                  # empty deps manifest
├── .qed/
│   └── findings/
│       └── 001-counter-monotonicity.md       # sample audit finding
└── program/
    ├── Cargo.toml
    └── src/lib.rs                            # FIXED Rust source
```

There is intentionally no `programs/` (qedgen-codegen output) or
`formal_verification/` (Lake project) checked in — the walkthrough
generates them as part of Step 3 so the reader sees what comes out
fresh.

## The idea

> *"I see this is an existing Solana program. Before we write a spec,
> let me hand off to `/qedgen-auditor` to find what's already broken.
> We'll lock those findings in as a spec so they don't come back."*

That framing — audit first, specify second — is what makes
brownfield adoption tractable. Spec-writing from a cold start is
unmotivated work; converting validated findings into permanent
regression guards is concrete value.

## Step 1 — Audit

From your harness (Claude Code TUI, Codex, Cursor, etc.), invoke
the auditor:

```
/qedgen-auditor
```

Expected output on the buggy variant (see Step 3 below for the
two-line diff that produces it): one fired HIGH at
`program/src/lib.rs:22` — `bump` violates monotonicity (the
counter walks downward instead of upward). The finding lands at
`.qed/findings/001-counter-monotonicity.md` (a sample is committed
in this example so you can see the shape without running the audit
end-to-end).

The audit ends with a handoff offer:

> "I helped find so many bugs, now let's get you to specify them so
> they never come back. Want me to draft a `.qedspec` codifying
> this finding?"

## Step 2 — Specify

Re-enter `/qedgen`. The agent reads `.qed/findings/001-…md`, looks
up the category (`broken_invariant`, Family 2) in
`skills/qedgen-auditor/references/finding_to_spec.md`, and drafts
`onboarding.qedspec` — committed at the root of this example. The
load-bearing property:

```
property counter_monotonic :
  state.counter >= old(state.counter)
  preserved_by all
```

The `old(...)` reference makes this a **Binary property** under
v2.23's pre/post lowering. The harness gets a real obligation —
`fn counter_monotonic(pre: &State, post: &State) -> bool { post.counter >= pre.counter }` —
instead of the pre-v2.23 silent tautology (`s.counter >= s.counter`).

Validate:

```bash
qedgen check --spec onboarding.qedspec
```

Expected: 0 P1 warnings (two informational P2/P3 about field bounds
and read-without-write are acceptable for a minimal demo).

## Step 3 — Verify (the red-then-green cycle)

Generate the harness:

```bash
qedgen codegen --spec onboarding.qedspec --target anchor \
  --output-dir programs \
  --proptest --proptest-output programs/tests/proptest.rs \
  --kani --kani-output programs/tests/kani.rs \
  --lean
```

Inspect `programs/tests/proptest.rs` — the key emission:

```rust
fn counter_monotonic(pre: &State, post: &State) -> bool {
    post.counter >= pre.counter
}

proptest! {
    #[test]
    fn bump_preserves_counter_monotonic(s in arb_state(), delta in 0u64..=u64::MAX) {
        let pre = s.clone();
        let mut post = s;
        if bump(&mut post, delta) {
            prop_assert!(counter_monotonic(&pre, &post),
                "counter_monotonic must hold after bump");
        }
    }
}
```

That `(pre: &State, post: &State)` signature and the
`let pre = s.clone(); let mut post = s;` capture are the v2.23
pre/post lowering working end-to-end. Pre-v2.23 the same property
lowered to `fn counter_monotonic(s: &State) -> bool { s.counter >= s.counter }`
— a structural tautology that reported green vacuously.

### Red path — inject the bug at the spec layer

The shipped spec's `bump` effect uses checked add: `counter += delta`.
The audit's bug was that the impl used `wrapping_sub` instead. Mirror
that into the spec by changing one line:

```diff
 handler bump (delta : U64) : State.Active -> State.Active {
   permissionless
   requires state.paused == 0 else Paused
   effect {
-    counter += delta
+    counter -=? delta
   }
 }
```

(`-=?` is wrapping-sub semantics — the spec analog of
`s.counter.wrapping_sub(delta)`.)

Regenerate and run:

```bash
qedgen codegen --spec onboarding.qedspec --proptest \
  --proptest-output programs/tests/proptest.rs
qedgen verify --spec onboarding.qedspec --proptest
```

Expected: `bump_preserves_counter_monotonic` fires red with a
proptest counterexample like:

```
counter_monotonic must hold after bump
  pre = State { counter: 0, paused: 0 }, delta: 1
  post = State { counter: 18446744073709551615, paused: 0 }
```

That's the same shape of bug the auditor flagged in `program/src/
lib.rs` — surfaced now as a structural obligation rather than a
post-hoc Mollusk repro.

### Green path — revert

Restore the effect to `counter += delta` and regen:

```bash
qedgen codegen --spec onboarding.qedspec --proptest \
  --proptest-output programs/tests/proptest.rs
qedgen verify --spec onboarding.qedspec --proptest
```

Expected: GREEN. The harness asserts the binary obligation against
the corrected effect.

## Step 4 — Stamp the impl

With the spec verified, the agent's next codegen step regenerates
`program/src/lib.rs`'s `bump` body from the spec:

```rust
pub fn bump(s: &mut State, delta: u64) -> Result<(), Error> {
    if s.paused == 1 { return Err(Error::Paused); }
    s.counter = s.counter.checked_add(delta).ok_or(Error::Overflow)?;
    Ok(())
}
```

(This matches the FIXED variant committed at `program/src/lib.rs`.)
Any future patch that re-introduces a `wrapping_sub`, swaps `+=`
for `-=`, or otherwise writes a value less than the pre-state into
`counter` fails the proptest run. The audit found the bug once; the
spec ensures it never comes back.

## Demo asymmetry — note for drift gates

This example is the only one in the bundled corpus whose value
proposition is "the obligation goes red then green," not "the test
stays green." `scripts/check-lake-build.sh --strict` and
`cargo test --release` see only the FIXED spec; the buggy variant
lives in this README as a documented one-line diff and never lands
on disk in a release-gated form.

If a future drift run regenerates the harnesses and the proptest
passes on the committed spec, that's the intended state — the
final-step state of the cycle, identical to a green Step 4.

## See also

- `skills/qedgen-auditor/references/finding_to_spec.md` — the
  conversion table the agent walks during Step 2.
- `skills/qedgen-auditor/SKILL.md` "Handoff to `/qedgen` for spec
  scaffold" — the auditor-side script.
- `SKILL.md` "First Contact (Brownfield)" — the qedgen-side branch
  that recommends the audit-first route on brownfield repos.
- `docs/prds/RELEASE-v2.23.md` Slice 8 — the source signals
  ([[feedback_audit_as_brownfield_wedge]],
  [[feedback_audit_first_finding_buys_time]]) and the design
  rationale.
