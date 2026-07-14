# Reproducer Contract

Use reproducers to test a concrete exploit claim, not merely whether an
instruction returns an error.

For each HIGH/CRITICAL candidate:

1. Construct pre-state satisfying every legitimate guard.
2. Identify the attacker-controlled accounts and inputs.
3. Invoke the actual program artifact through `qedgen-sandbox`.
4. Assert the security violation as a post-state delta, authority change, fund
   movement, or availability failure.
5. Run `qedgen verify --probe-repros --json` and retain the assignments.

An expected program error is evidence of a vulnerability only when the claim is
denial of service and the pre-state represents a valid operation that should
succeed. A test that merely observes rejection does not prove theft, takeover,
or corruption.

Outcomes:

- Fired security assertion: confirmed.
- Simulator/build limitation with independently established reachability:
  structural.
- Simulator/build limitation without established reachability: hypothesis.
- Security assertion does not fire: rejected.

Keep reproducers under `target/qedgen-repros/audit/`; do not commit them unless
the user explicitly converts them into permanent regression tests.
