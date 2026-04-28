# Skill Operations

This file keeps operational guidance out of `SKILL.md` while preserving the
details agents need during longer engagements.

## Learning Capture

Use `.qed/plan/` for durable local context when a project spans sessions:

- Record the verified scope.
- Record deferred properties and why they are deferred.
- Record proof backend failures and next actions.
- Record handler ownership decisions.

Do not treat notes as proof. Revalidate with `qedgen check`, build commands,
and backend verification.

## Git Hygiene

Before codegen or large edits:

```bash
git status --short
```

Never overwrite user-owned handler bodies, `Proofs.lean`, or existing tests
without explicit user intent. If generated support code drifts, regenerate it
with QEDGen rather than hand-editing unless debugging the generator itself.

## Environment

API keys and Lean tooling are not required for spec linting or Rust codegen.
They are only needed for proof filling and Lean builds.

Useful checks:

```bash
qedgen --help
lake --version
cargo-kani --version
```

## Error Handling

If `qedgen check` reports lint issues, fix the `.qedspec` first.

If generated support code fails to compile, fix the generator or generated
support surface.

If handler code fails because of `todo!()`, fill the handler business logic.

If Lean reports missing or orphan theorems, update `Proofs.lean` or reconcile
the `.qedspec` change. Do not silently delete proofs to make the report clean.
