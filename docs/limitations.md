# QEDGen — known limitations

A running list of constructs the spec parser accepts but downstream codegen
can't (yet) lower to a non-vacuous harness. Each entry includes the lint
`qedgen check` emits when the shape appears, plus the supported workaround.

## Quantifier shapes (v2.20 §S1.1)

QEDGen lowers `forall <binder> : <T>, body` to a per-slot predicate
`<prop>_at(s, <binder>)` plus a Kani / proptest harness that binds
`<binder>` symbolically (`kani::any::<T>()` / `any::<T>()`). That covers
**single-binder forall** over any of:

- Integer primitives (`U8…U128`, `I8…I128`)
- `Bool`
- `Fin[N]` (bounded index type)
- Named record / sum / lifecycle-state types declared with `type`

Shapes that *don't* fit the above produce the P5
`unsupported_quantifier_shape` lint at `qedgen check` time and skip the
harness emission for that property. The categories below trip the lint.

### `nested-quantifiers`

```
property foo : forall d : Distribution, forall c : Claim, P(d, c)
```

**Why it fails:** the harness model binds *one* symbolic value per
property; nested quantifiers need a Cartesian product the BMC / proptest
loop can't enumerate in a single test.

**Workaround:** split into two single-binder properties.

```
property foo_outer : forall d : Distribution, P_d(d)
property foo_inner : forall c : Claim,        P_c(c)
```

If the body genuinely needs both at once (e.g. `P(d, c)` doesn't
factor), use a record literal to combine them into one binder type:

```
type Pair = { d : Distribution, c : Claim }
property foo : forall p : Pair, P(p.d, p.c)
```

### `unbounded-binder`

```
property foo : forall v : Vec<U64>, sum(v) >= 0
property foo : forall v : List<Account>, …
```

**Why it fails:** `Vec<T>` / `List<T>` / `Set<T>` are unbounded; neither
Kani's symbolic execution nor proptest's strategy generation can enumerate
all possible values.

**Workaround:** use `Map[N] T` for a bounded collection (stored as a
spec field rather than a binder), or bound the quantifier to one element
via a primitive index:

```
type AccountMap = Map[MAX_ACCOUNTS] Account
type Idx = Fin[MAX_ACCOUNTS]

property foo : forall i : Idx, s.accounts[i].balance >= 0
```

### `exists-quantifier`

```
property witness : exists d : Distribution, d.balance > 0
```

**Why it fails:** v2.20 only lowers `forall`. Witnessing an `exists`
needs a constructive proof at the harness level — currently outside the
mechanical-codegen contract.

**Workaround:** rephrase as the matching `forall` invariant if the
intent is "every element satisfies P":

```
property all_positive : forall d : Distribution, d.balance > 0
```

If the intent is genuinely "there is at least one" (a liveness-style
statement), encode it as a `cover` or `liveness` declaration instead of
a property.

---

## Pubkey state fields (v2.20 §S1.3)

`Pubkey`-typed state fields are not supported by v2.20 codegen. Declaring
one on a State type produces the P6
`pubkey_state_field_unsupported` lint at `qedgen check` time.

```
type State
  | Active of {
      authority : Pubkey,   // ← P6
      balance   : U64,
    }
```

**Why it fails:** the proptest harness generator filters Pubkey fields
out of the generated `struct State { ... }` (no clean `proptest::Arbitrary`
lowering for `Pubkey` in the current pipeline), while handler bodies still
emit `s.authority = authority`. Net effect: a generated `tests/proptest.rs`
that doesn't compile (`no field 'authority' on State`).

**Workaround — model the value as a handler parameter** instead of
storing it in state. If the pubkey doesn't need to persist across calls
— e.g. a one-shot admin check at handler entry — pass it as a parameter
rather than declaring it as a state field.

```
handler set_admin (new_admin : Pubkey) : State.Active -> State.Active {
  // Pubkey-as-parameter is supported; the elision bug is state-only.
  // No state field needed.
}
```

`Pubkey`-typed handler parameters are accepted by `qedgen check` and
codegen; the constraint is specifically on `field : Pubkey` inside
declared state.

**Important — no in-state workaround in v2.20.** A natural reflex is to
replace `field : Pubkey` with a 32-byte array (`Pubkey`'s on-chain
representation), but the spec grammar in v2.20 has **no array type**.
`[u8; 32]` doesn't parse. If the value genuinely must persist in state
(e.g. it's the identity of an open escrow, not a one-shot input), the
only honest answer for v2.20 is that the spec can't express it cleanly
— either lift the state to a sum-typed lifecycle that closes when the
identity becomes irrelevant, or wait for v2.21.

**What v2.21 may change:** two paths under consideration. (a) Add a
fixed-size byte array type (`Bytes32` / `[u8; N]`) to the spec
grammar so the in-state workaround actually parses. (b) Option B of
the S1.3 ADR — lower `Pubkey` state fields to `[u8; 32]` in the
generated Rust structs automatically. Either lifts the lint
materially; until one ships, P6 is the user-visible gate and "move
to handler parameter" is the only working in-spec answer.

---

## `declare_id!` without a `program_id` (#368)

Codegen must emit a `declare_id!`, so a spec that declares no
`program_id` falls back to `11111111111111111111111111111111` — the
System Program's address. `qedgen init` templates that same value into
new specs, so the default authoring path produces it too.

The trap is that it is a **valid base58 pubkey**. Nothing downstream
rejects it on shape, so it reads back as a real address:

```
declare_id!("11111111111111111111111111111111");   // the System Program
```

Three things now make it visible rather than silent:

- `qedgen check` reports the P3 `missing_program_id` info, for both the
  absent case and a verbatim placeholder;
- the generated `lib.rs` carries a `// PLACEHOLDER:` comment above the
  line, which is the only in-band signal a reader or a grep can use;
- the probe reproducer lane refuses to build against it, rather than
  aiming an attack transaction at the System Program and reporting "no
  bug" for an unrelated reason.

Info rather than Warning on purpose: `qedgen check` exits non-zero on
any Warning, and a spec legitimately has no deployed address for most
of its life. Gating ordinary in-progress work on "you have not deployed
yet" would get the rule muted rather than obeyed.

Fix: add `program_id "<your id>"` to the spec, from `anchor keys list`
or `solana-keygen pubkey target/deploy/<name>-keypair.json`.

Every bundled example under `examples/rust/` now declares a real
`program_id` (generated with `solana-keygen`, one per example, with the
imported `cross-program-vault-admin` getting its own). They demonstrate
the practice rather than the placeholder.

Note for maintainers: `program_id` is a top-level spec item, so it feeds
`spec_context_digest` and therefore every handler's `spec_hash`. Changing
one re-stamps every `#[qed(verified, …)]` in that program and moves the
expectations in `crates/qedgen-hash-core/tests/stamp_crosscheck.rs`. This
is by design — that digest is deliberately conservative — but it makes a
`program_id` edit a wider change than it looks.

---

## Past limitations (lifted)

- **`forall` over wider-than-U8 binder types verified vacuously** —
  fixed in v2.20 §S1.1. The harness layer now binds the symbol via
  `kani::any` / proptest `any::<T>()` and calls `<prop>_at(&s, <binder>)`
  instead of the legacy `<prop>(&s) ⟶ true` stub.

## Reporting

When `qedgen check` emits a P5 lint pointing at one of the categories
above and you've hit a *new* shape that should be supported, open an
issue at <https://github.com/qedgen/solana-skills/issues> with the
fragment that doesn't lower.

**Agents authoring `.qedspec`s: fail fast.** SKILL.md's "When the
spec cannot go forward" section plus `references/filing-issues.md` are
the binding contract — if you hit a lint
/ codegen error (or a generated-output compile / kani / lake failure)
not covered by the documented workarounds above, sanitize a generic
reproducer, ask the user before filing, then file the issue and
pause. Do not invent phantom state fields, hand-edit generated files,
or otherwise lie about the spec to silence the lint. The workaround
lives in the spec forever; the bug doesn't.

**Sanitization is mandatory.** GitHub issues are public. Strip real
pubkeys, named accounts / fields / handlers that hint at protocol
identity, deal-specific constants, internal paths, and product-name
comments before sharing. The issue only needs the generic shape that
trips qedgen — not the user's business logic. See SKILL.md step 3
for the full scrub checklist.
