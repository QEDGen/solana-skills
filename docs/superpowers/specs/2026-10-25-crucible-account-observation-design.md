# Crucible account observation design

## Intent

Make a spec state assertion read stored Solana account data after each action. The existing accepted argument mirrors remain a separate, limited form of evidence. This first slice supports named Borsh Anchor accounts with primitive scalar fields only.

## Approaches

1. Generate reads from inferred names: concise, but a state field, instruction account, and IDL field may have different names. Rejected.
2. Require explicit, versioned bindings of each spec field to an IDL account type and field, plus a handler account role. Chosen; it makes identity and layout reviewable.
3. Interpret arbitrary IDL layouts dynamically in the harness: broader but harder to verify and more permissive of unsupported layouts. Deferred.

## Contract

`--crucible-state-mapping <path>` accepts schema version 1 JSON with `fields` mapping spec field names to `{account_type, data_field, handlers}`. `handlers` maps every spec handler name to the declared instruction account role that carries this state. Missing handler roles are rejected before generation: each action selects its own mapped account identity, and the observed fixture field is the projection of that action's account. The mapping also requires an ordered `setup` array of `{handler, args}` calls. The generated fixture executes those real program instructions before fuzz dispatch and requires each to succeed. This first slice accepts exact primitive scalar arguments only. Mapping is valid only for a spec-based Crucible probe. It is validated against parsed spec and installed Anchor IDL before generation and joins the generation fingerprint.

For example: `{"schema_version":1,"fields":{"count":{"account_type":"Counter","data_field":"count","handlers":{"initialize":"counter","increment":"counter"}}},"setup":[{"handler":"initialize","args":{}}]}`. Each handler role must select an account initialized by the setup calls. A role that selects a different missing account causes a clear observation failure.

The generated action uses the exact pubkey already emitted in its `.accounts(...)` literal, including PDA and account overlay resolution. After sending, it retrieves that account from LiteSVM, validates existence and nonzero lamports, program owner, complete IDL discriminator, and Anchor Borsh decode. The mapped scalar is copied into the fixture only after every check succeeds. An observation error has a dedicated diagnostic and fails the probe as incomplete, with a nonzero CLI exit; it must never be counted as a named invariant violation. The observation happens after every action, irrespective of declared spec effects or transaction success. The ordinary accepted input mirror never writes a mapped field.

Only bool and signed/unsigned 8/16/32/64/128 bit scalars with equal spec and IDL types are supported. Account type must be a named Borsh struct, with no zero-copy representation or generics. Unsupported account layouts and field types fail validation. Missing or closed accounts fail observation explicitly.

Existing source-bound manual readiness reviews can still acknowledge fields without a mapping, but a valid mapping removes only its own field blocker. A generated harness with changed mapping must be regenerated; source/IDL/binding freshness prevents silently using prior code.

## Verification

Unit tests cover mapping validation, owner/discriminator/absence guard emission, readiness and mirror separation, and freshness. A live Anchor fixture initializes actual stored state, then a buggy update violates an invariant while a corrected update stays clean. The old domain boundary fixture is not stored-state evidence.
