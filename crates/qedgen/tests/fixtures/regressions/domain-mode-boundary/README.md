# Protocol/domain Crucible boundary regression

This fixture demonstrates a domain bug that the generic protocol suite cannot
identify. The deployed `accept(amount)` instruction succeeds for every `u64`
and does not mutate lamports, ownership, allocation, discriminators, or token
balances. Its writable signer makes the protocol checks non-vacuous, but they
still report no finding.

The ratified dossier caps accepted amounts at 10. Domain mode links that fact
to `accepted_amount_within_domain` and deterministically replays `amount = 11`.
Because the instruction succeeds, the harness mirrors the accepted scalar into
its domain shadow state and the invariant fires. The one-action replay seed is
14 bytes: Crucible's action envelope plus the little-endian `u64` argument.

The normal integration test exercises both emit paths and pins the exact replay
seed. Set `QEDGEN_RUN_LIVE_CRUCIBLE_DOMAIN_BOUNDARY=1` to run the SBF program and
assert that protocol mode stays clean while domain mode emits an
`invariant_violation` and durable replay evidence.
