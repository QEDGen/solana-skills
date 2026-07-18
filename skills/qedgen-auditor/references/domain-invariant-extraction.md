# Domain invariant extraction

Use this pass independently of `qedgen probe`. Its output is useful for manual
review, intent ratification, spec authoring, and later fuzzing even when tooling
or compilation is blocked.

## Output: domain dossier

Write `.qed/audit/<timestamp>/domain-dossier.json`, validate it against
`../schemas/domain-dossier.schema.json`, and render
`.qed/audit/<timestamp>/domain-dossier.md` for the user. The JSON is canonical;
the Markdown must not introduce candidates or ratification decisions absent
from it. Retain source citations for every candidate and organize both forms
into six sections:

1. **Asset-flow graph:** handler, asset, source, destination, nominal amount,
   delivered amount, fees, authority, and corresponding state mutation.
2. **Quantity and unit table:** field or parameter, semantic unit, scale,
   rounding direction, valid range, and conversions. Distinguish tokens, shares,
   lots, prices, basis points, slots, timestamps, indexes, and raw integers.
3. **Lifecycle graph:** states, allowed edges, entry guards, effects, terminal
   states, reversible edges, and parent/child account dependencies.
4. **Authority-capability matrix:** role, authenticated identity, allowed
   handlers, mutable fields, external capabilities, limits, and revocation path.
5. **Economic equations:** candidate conservation, solvency, pricing, accrual,
   allocation, and monotonicity properties, including fees and tolerated error.
6. **External assumptions:** oracle semantics, token extensions, CPI guarantees,
   clock/epoch assumptions, keeper behavior, governance, and trusted libraries.

For each entry record a stable `id`, `candidate`, `source anchors`, `confidence` (`literal`,
`derived`, or `semantic`), `ratification` (`auto`, `user`, `rejected`, or
`pending`), and `verification lanes` (`manual`, `Mollusk`, `Miri`, `Crucible`).
Never reuse an ID for a semantically different candidate within the same audit.
When `qedgen probe --emit-spec-candidates --audit-dir <dir>` succeeds, it seeds
the canonical JSON with `structural_candidates` derived from probe clusters.
Preserve those IDs while enriching the six domain sections; do not copy them
into new IDs merely because the confidence or ratification state changes.
`qedgen ratify --audit-dir <dir>` persists `user`, `rejected`, and `bug`
outcomes back onto those same candidates. Unanswered candidates remain
`pending`; rejected and bug outcomes retain the interview rationale.

The binary's hypothesizer now pre-seeds seven classes as evidence-anchored
`hypotheses[]` in the probe envelope (deterministic, precision-first: no
evidence anchor, no hypothesis): authorization, lifecycle/init-once,
arithmetic-bound (held checks lifted from bodies), conservation (paired
operations, no supply-changing flows), CPI-integrity (pinned callee +
resolved transfer roles), unwired-guard (dead error variants as
confirm/deny questions), and state-machine (the IDL status enum lifted
into `type State`). Verify and extend those records rather than
re-deriving them; the agent owns everything the binary abstained on —
ambiguous cases and the deep cross-procedure classes in the sections
above.

Maintain `.qed/audit/<timestamp>/run-manifest.json` against
`../schemas/audit-run-manifest.schema.json`. It is the canonical status of
source review, ordinary probe, compilation, Mollusk, Miri, and each Crucible
entry point. A blocked lane records both why it is blocked and the exact command
that resumes it.

## Extraction procedure

1. Inventory state types, handler signatures, account constraints, events,
   errors, constants, tests, and documentation.
2. Trace each value-moving handler from external effect to internal accounting.
   Compare requested amounts with measured delivered amounts.
3. Pair converse operations: deposit/withdraw, mint/redeem, borrow/repay,
   open/close, freeze/thaw, nominate/accept, create/destroy, and settle/reopen.
4. Infer equations only after assigning units. Reject equations that add or
   compare incompatible units without an explicit conversion.
5. Build the authority matrix from both signer constraints and stored identity
   anchors. A signer proves authentication, not entitlement to every effect.
6. Compare implementation candidates with tests and prose containing `must`,
   `only`, `always`, `never`, caps, windows, ratios, and ordering claims.
7. Auto-ratify only literal properties anchored by an enforcement line.
   Present derived and semantic candidates to the user with previews.
8. Preserve rejected candidates and the reason; do not silently recycle them
   into generated specs or fuzz assertions.

## Domain prompts

Select only packs matching the program; these are prompts for extraction, not
invariants to assume.

- **AMM/CLMM:** curve or tick invariant, fee placement, liquidity accounting,
  price limits, reserve/token balance agreement, and rounding per swap direction.
- **Lending:** indexed debt, utilization/accrual timing, solvency, collateral
  factors, liquidation close factor/bonus, bad-debt ordering, and oracle bounds.
- **Vault:** share pricing reference point, donation behavior, fee timing,
  total-assets definition, withdrawal liquidity, and share-price monotonicity.
- **Perpetuals:** margin tiers, PnL realization, funding accrual, oracle choice,
  liquidation ordering, insurance fund, and socialized-loss waterfall.
- **Auction:** price curve, bid ordering, escrow conservation, settlement
  finality, cancellation window, and unsold-asset recovery.
- **Rewards/vesting:** funded allocation, eligibility snapshot, vesting clock,
  claimed/revoked accounting, clawback authority, and close dependencies.
- **Governance:** voting-power snapshot, delegation, quorum, proposal lifecycle,
  timelock, execution replay, and authority transfer.
- **Oracle-dependent:** freshness, confidence, aggregation, fallback precedence,
  circuit breakers, unit/decimal conversion, and manipulation window.

## Ratification questions

Ask semantic questions with concrete alternatives and cited code, for example:

- "Deposit mints shares from pre-deposit assets, while redeem prices from
  post-fee assets. Is that the intended reference point?"
- "Liquidation currently permits 100% debt repayment per call. Is the intended
  close factor 100%, 50%, or state-dependent?"
- "The state records requested transfer amount, but Token-2022 may deliver less.
  Should accounting use nominal or measured balance delta?"

Do not ask the user for handler names, field names, account identities, or facts
already established by source. A ratified answer becomes a domain property; an
unanswered question remains a hypothesis and must not inflate vulnerability
totals.

## Probe-failure behavior

When ordinary probe or compilation fails, finish the dossier and record the
blocked lane, reason, and resume command in the run manifest. Continue manual authority, lifecycle, accounting,
paired-operation, and intent-drift passes. Draft specs as `pending verification`
and retain commands needed to resume. When tooling recovers, validate syntax,
run targeted Mollusk/Miri tests, then run the applicable Crucible entry point.
