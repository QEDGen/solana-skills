# `#[qed]` drift-loop fixture

This is the v2.9 acceptance regression for the brownfield drift loop.
It's a tiny crate — two `pub fn`s annotated with
`#[qed(verified, spec=, handler=, hash=, spec_hash=)]` against the
spec at `example.qedspec`. Workspace `cargo build` compiles it on
every run, so any drift between the spec, the source, or the two
hashing implementations (`qedgen-macros` ↔ `qedgen::spec_hash`)
shows up as a compile failure in CI.

## What it pins

| Side                  | What it computes        | What it checks                     |
|-----------------------|-------------------------|------------------------------------|
| `qedgen-macros::content_hash` | body hash from `ItemFn` tokens | matches the `hash = "..."` in source |
| `qedgen::spec_hash::body_hash_for_fn` | same algorithm | what `qedgen adapt --spec` would emit  |
| `qedgen-macros::spec_bind::spec_hash_for_handler` | extracted `handler { ... }` block | matches the `spec_hash = "..."` in source |
| `qedgen::spec_hash::spec_hash_for_handler` | same algorithm | what `qedgen adapt --spec` would emit |

If either pair drifts, this crate stops compiling and the workspace
test run fails.

## Demonstrate it

```bash
# Success path — pinned hashes match the recomputed values.
cargo build -p qed-drift-fixture
#   Finished `dev` profile in 0.4s.

# Tweak a body. Drift fires.
sed -i.bak 's/amount + 1/amount + 2/' src/lib.rs
cargo build -p qed-drift-fixture
#   error: qed: verified function `deposit` has changed since verification …
#          Expected: ac26f349ac12dd3e
#          Actual:   81db342cb120f686

# Revert the edit. Drift clears.
mv src/lib.rs.bak src/lib.rs
cargo build -p qed-drift-fixture
#   Finished
```

## Refreshing hashes after intentional edits

The proc-macro emits the freshly computed hashes when either is
absent. So:

1. Clear the `hash = "..."` and `spec_hash = "..."` strings (or
   delete those args entirely).
2. `cargo build -p qed-drift-fixture` — read the computed hashes
   off the error message.
3. Paste them back in.
4. Build again — succeeds.

For real Anchor programs, `qedgen adapt --program <crate>
--spec <path>` does this in bulk.
