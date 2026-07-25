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

## Filing Feedback

When the user hits qedgen itself — not a missing handler body or a spec they
can fix from the lint message — point them at `qedgen feedback`. It bundles
the last command's stderr, the relevant `.qedspec` excerpt, the qedgen
version, OS/arch, and detected runtime into a GitHub issue. Local copy is
always written to `.qed/feedback/<timestamp>.md`; the remote submit is
gated on an explicit y/N (or `--yes` in non-interactive shells).

Surface the command proactively when any of these fire:

- **Same lint or codegen error appears twice in the session without progress.**
  Two attempts at the same problem with no progress — they are blocked on something that
  may not be their bug. Suggest `qedgen feedback --note "<one-line summary>"`.
- **An internal qedgen error or panic.** Stack traces, "unreachable", parser
  errors that aren't user-fix-able, file-not-found on paths qedgen owns. The
  message is not actionable by the user; the maintainers need the trace.
- **Frustration signals in conversation.** "this is broken", "why doesn't
  this work", "I've tried everything" — soft signal, but worth a one-line
  offer: _"Want me to draft a `qedgen feedback` issue with the last error's
  context?"_

Skip the suggestion when the failure is clearly user-side (typo in spec, missing
dependency, wrong handler signature). Don't suggest it more than once per session
unless a new class of error appears.

Always preview the body with `--dry-run` first if the spec might contain
proprietary business logic — the user gets to redact before the issue is filed.
