# Data-structure dep invariant checklist

Companion reference for SKILL.md §3c (Trust-surface dep walk). Read on
demand when §3c lands on a niche data-structure or algorithmic dep that
fund movement leans on — zero-copy collections, custom traversal
primitives, sequence-number primitives in hot paths where memory safety
+ invariant preservation matter for state-machine correctness.

Unlike the crypto-primitive walk in `trust_surface_primitives.md`,
which is anchored on a stated security property (EU-CMA, binding,
soundness), data-structure deps imply their contract via API shape.
Each axis below makes that implicit contract explicit. Ask the listed
question of the dep's `impl` blocks and `unsafe` regions (not its
tests). Pattern-match against the named bug shapes.

---

## 1. Iteration soundness

**Question.** Can any iterator the dep exposes (`iter_mut`, `drain`,
`range_mut`, `DoubleEndedIterator`) yield two references aliasing the
same value within a single session, or survive a structural mutation
of the container mid-iteration?

**Bug shapes:**

- **`next` / `next_back` cursors crossing.** Two independent cursors
  walking from each end with no shared termination check. When they
  meet or pass, the same element is reachable from both directions —
  two `&mut`s to one cell.
- **Iterator-invalidation under mutation.** Iterator caches a node
  pointer / length / version on construction; caller mutates the
  container before consumption; cached pointer dangles or stale-reads.
  Raw-pointer-based zero-copy deps often hand out `&mut V` with a
  lifetime detached from `&mut self`, so the borrow checker doesn't
  catch the dangle.

**Why it matters in a Solana program context.** Zero-copy structures
back program state directly into account bytes; an aliased `&mut` is
UB and lets one instruction write conflicting updates to one logical
slot — silent corruption that lands directly in the state-machine
invariant. Exploit shape: handler iterates a position list and CPIs
inside the loop; CPI touches the same container; iterator's cached
pointer drifts; attacker grinds the instruction sequence that forces
the re-org.

---

## 2. Ordering invariant preservation

**Question.** After every insert, remove, and replace operation, does
the dep's claimed ordering (sort key, FIFO, LIFO, priority) still hold
for every traversal API?

**Bug shapes:**

- **Replace-in-place that bypasses comparator.** A method like
  `entry().or_insert_with()` or `get_mut()` returns `&mut V`; caller
  mutates the field that participates in the sort key without
  re-positioning. Subsequent `range()` or `iter()` walks the tree in
  stale order.
- **Removal that leaves a sentinel without re-linking.** Tombstone
  entries that the iterator still surfaces, or skip-list lanes whose
  back-pointers weren't updated.

**Why it matters in a Solana program context.** Orderbook matching,
liquidation priority queues, FIFO vesting unlocks — all assume
"first/best" semantics from the dep. A broken ordering invariant
means the program transitions against the wrong element: pays the
wrong vester, matches a stale price level, liquidates the wrong
account.

---

## 3. Re-balancing correctness (for trees)

**Question.** For self-balancing trees (RB, AVL, splay, B-tree
variants), does every rotation / split / merge preserve the structural
invariant in every parent/child color/height combination?

**Bug shapes:**

- **Missing case in `delete_fixup`.** The classic RB-tree deletion
  has six cases mirrored across left/right siblings; one missed case
  leaves a red-red violation that compounds over subsequent inserts.
- **Off-by-one in subtree-size or rank metadata.** Order-statistic
  trees that track `size` or `rank` per node; a rotation forgets to
  recompute, and `select(k)` returns the wrong element thereafter.

**Why it matters in a Solana program context.** A broken balance
invariant degrades O(log n) operations to O(n) and eventually trips
compute-unit limits — a slow-burn DoS on long-lived state. Worse, if
`select(k)` or `rank(v)` participates in authorization (e.g., "top-N
depositors get priority"), the wrong element gets the privilege.

---

## 4. Capacity-edge behavior

**Question.** What does the dep do at `len == capacity` for insertion,
at `len == 0` for removal, at index `capacity - 1` for indexed access,
and at wrapping points for sequence-number primitives?

**Bug shapes:**

- **Silent overwrite at full capacity.** Fixed-size ring buffer or
  zero-copy slab returns `Ok(())` from `push` after eviction, with no
  signal that the evicted element existed. Caller assumes
  insertion-only.
- **Wrap-around on a u32/u64 sequence number.** Generation counter
  that wraps to zero collides with sentinel "empty" or "version 0"
  values; later code treats the wrapped entry as fresh.

**Why it matters in a Solana program context.** Fixed-capacity zero-
copy structures are the norm on Solana (account size is bounded;
realloc is expensive), so the edge case is the steady state of any
popular program. Sequence-number wrap is the classic "ran fine for
two years and then catastrophically failed on a Tuesday" bug —
mainnet-only, attacker-grindable.

---

## 5. Shared-mutable / interior-mutability soundness

**Question.** Does the dep expose `&self` methods (not `&mut self`)
that mutate underlying storage via `UnsafeCell`, raw pointer casts,
`bytemuck::cast_slice_mut`, or `RefCell`-like wrappers — and does
each such method enforce its own non-aliasing invariant?

**Bug shapes:**

- **`&self` method returning `&mut V` through a raw cast.** Two
  callers holding `&self` simultaneously each obtain `&mut V` to
  overlapping memory. The dep's `Sync` impl is unsound; the program
  trips UB the first time a handler holds two account refs that
  resolve to the same byte range.
- **`RefCell`-style runtime borrow check that panics-not-errors.**
  Inside a handler, panic aborts the transaction with a cryptic
  log; an attacker who can force the double-borrow path has a
  guaranteed DoS lever on the entire program.

**Why it matters in a Solana program context.** Zero-copy account
loaders hand out `&mut T` from `&AccountInfo` via `unsafe`; the dep
may launder that `&mut` further through its own API. Aliasing rules
*compose* — both pieces can be individually sound and the composition
unsound. This is exactly the class of bug Miri catches (see v2.19
Pinocchio probe / Miri verify backend); if the dep ships its own test
suite, check whether it runs under Miri.

---

## Corpus

**Zero-copy red-black-tree `DoubleEndedIterator` (pre-fix).** A
widely-used zero-copy data-structures crate. The `iter_mut` method exposed a
`DoubleEndedIterator` whose `next` and `next_back` tracked separate
cursors with no shared termination check. Once the cursors crossed,
the same value was reachable from both ends within a single
iteration — yielding two simultaneous `&mut V` to the same byte
range. Any program calling `iter_mut().next_back()` adjacent to
`.next()` tripped UB. Fix: make the cursors aware of each other
(shared termination) or restrict `iter_mut` to single-ended
iteration. Discovery shape: trust-surface walk of a matching engine
using `iter_mut` on the RB-tree for in-place price-level updates —
auditor reads the iterator `impl`, spots the absent shared
termination, fires a Mollusk repro by replaying two adjacent fills
through the program's API path. An internal empirical study walked
past this bug because the v2.19 §3c gate only triggered on crypto
verbs (`sign`, `verify`, `prove`, `commit`) — the zero-copy
data-structure dep is the gap §S3.5 closes.

---

## How to surface this in §3c

When §3c surfaces a small / niche data-structure or algorithmic dep
that fund movement leans on — and the dep doesn't match the crypto-
verb signals in `trust_surface_primitives.md` — before declaring it
out-of-scope, walk the five axes above against the dep's source. If
any axis surfaces a candidate bug, follow the standard finding flow:
file a Mollusk repro through the program's API path, anchor in the
dep's `impl` (cite file + line), rate severity by permissionless
reachability.

If none of the axes apply because the dep occupies a class not yet
listed, audit from first principles (what implicit contract does the
API shape state?), then add the new axis here using the question /
bug-shapes / "why it matters" template. The *process* (locate
implicit contract → enumerate failure modes → verify in source) is
invariant; the per-class axis lists are the artifact it produces.
