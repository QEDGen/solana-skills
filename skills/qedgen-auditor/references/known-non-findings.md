# Known Non-Findings

Patterns that are reported as Solana vulnerabilities but are not, because the
runtime, the token program, or the framework already prevents the claimed
attack. Read this before filing a finding in any of the classes below. Filing
one of these as a vulnerability costs the audit its credibility on the findings
that are real.

Each entry states the claim, why it does not hold, and the narrow condition
under which a related issue would be real. The narrow condition is the part
worth investigating. It is usually a different bug than the one the pattern
match suggested.

These are runtime and version dependent. Confirm what the target's actual
Solana version, Anchor version, and token program enforce before rejecting on
the strength of this file alone. The same discipline the catalog applies to
framework wrappers applies here: the guarantee is evidence, and evidence has a
version.

## Reentrancy across a CPI

**Claim.** A handler that performs a CPI needs reentrancy protection, because
the callee can call back in and re-enter before state is committed.

**Why it does not hold.** The runtime rejects a CPI into a program that already
appears on the invocation stack. `A -> B -> A` returns `ReentrancyNotAllowed`.
The instruction stack depth is capped at 5, or 9 with SIMD-0268.

**Where something real might be.** Direct self-recursion, `A -> A -> A`, is
explicitly allowed and the rule above does not cover it. Separately, a callee
that mutates accounts the caller re-reads afterwards is a real problem, but it
is staleness rather than re-entry, and belongs under
`account_not_reloaded_after_cpi`. Untrusted code executing inside a caller's
critical section is also real and is also not re-entry: that is
`transfer_hook_untrusted_callback`, which was named `transfer_hook_reentrancy`
until 2026-07-30 and described a callback path the runtime does not permit.

## Anchor closed-account discriminator

**Claim.** Closing an Anchor account safely requires writing a special closed
account discriminator, and a program that does not is vulnerable.

**Why it does not hold.** Anchor has no closed-account discriminator. The
mechanism it refers to was removed in Anchor v0.30.0.

**Where something real might be.** Account closure is still sensitive, and the
underlying concern is legitimate: lamports must move out, data must be zeroed,
and ownership must return to the system program. A native program doing this by
hand can get it wrong. File that as `pda_lifecycle_reuse_after_close` or
`close_account_redirection` with the actual missing step named. Do not file the
discriminator itself.

## Float non-determinism

**Claim.** Floating point in a Solana program is non-deterministic across
validator hardware and risks a consensus split.

**Why it does not hold.** Floating point operations are emulated with the
available sBPF opcodes rather than executed on host floating point units, so
there is no hardware level variance at the application layer.

**Where something real might be.** Accuracy is a genuine concern. Interest and
ratio math in floats produces precision loss, and can produce NaN or infinity
that then propagates into a stored balance. Token-2022's scaled UI amount
extension is a live example of float arithmetic that needs review. File the
specific numeric failure, not determinism.

## Token self-transfer

**Claim.** A token transfer where source and destination are the same account
always succeeds, so a user can bypass a fee, a staking requirement, or a
cooldown by transferring to themselves.

**Why it does not hold.** The token program is not short-circuiting. Mints must
match, the source must be solvent, and neither account may be frozen. The
transfer succeeds only after those checks pass, so it is not a free bypass.

**Where something real might be.** Whether source equals destination matters to
the calling program's own accounting is a design question worth asking. If the
handler computes a delta or a fee assuming two distinct accounts, that is
`duplicate_mutable_accounts_aliasing` and belongs under that category.

## `load_instruction_at` and `load_current_index`

**Claim.** These instruction-introspection calls let an attacker supply a
forged sysvar and manipulate logic that depends on instruction ordering.

**Why it does not hold.** The unchecked variants were fixed in 2022 and then
removed. Contemporary code cannot call them.

**Where something real might be.** The general shape is alive and is the reason
`account_type_confusion` is CRITICAL: any well-known account passed as a bare
`AccountInfo` instead of its strongly typed wrapper can be substituted. Check
the account typing, not the function name.

## Partial state commitment on a failed transaction

**Claim.** A transaction that fails partway can leave some accounts updated and
others not, producing inconsistent state.

**Why it does not hold.** Solana rolls back all account state when a
transaction reverts. The commitment is all or nothing.

**Where something real might be.** Nothing at the transaction level. Multi
transaction flows are a different matter: a protocol that requires two separate
transactions to reach a consistent state can be interrupted between them. That
is a lifecycle finding, not a commitment finding.

## Unchecked CPI return values

**Claim.** The program must check the error returned by a CPI or it will
continue running in a corrupted state.

**Why it does not hold.** A CPI error aborts the whole transaction. There is no
catch, unlike the EVM. Omitting an explicit error check does not let execution
continue.

**Where something real might be.** Reading a callee's effect from a return
value rather than from the modified accounts is a real correctness question,
and so is failing to reload an account the callee changed. Both belong under
`account_not_reloaded_after_cpi`.

## Still real

Two adjacent patterns are not on this list because they are genuine.

Stack exhaustion in deeply nested Anchor account structs is real, and the
mitigation depends on the Anchor and runtime versions in use.

Duplicate mutable accounts is real. Anchor has historically allowed the same
account to be passed for two mutable parameters of the same type. The catalog
covers it as `duplicate_mutable_accounts_aliasing`.

## How to use this in a report

A pattern on this list is `rejected` under the evidence rules, with the reason
recorded as the specific guarantee that prevents it. Name the guarantee. A
report that says "rejected, see known non-findings" is not traceable; one that
says "rejected: the runtime rejects a CPI into a program already on the stack"
is.

Do not carry these into the open-questions section either. An open question is
something unresolved. These are resolved.

If the target's runtime or framework version predates the fix that makes one of
these safe, then it is not on this list for that target. Say which version you
checked.
