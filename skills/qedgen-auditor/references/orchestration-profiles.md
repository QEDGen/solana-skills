# Orchestration Profiles

## Quick

Use one focused pass. Prioritize authority, account identity, CPI targets,
arithmetic around fund movement, and obvious lifecycle violations. Attempt
cheap HIGH/CRITICAL reproducers. Clearly state omitted coverage. Use the
balanced model tier from `model-selection.md`.

## Standard

Use one complete pass over every applicable category plus cross-handler
authority, state-machine, safe-helper coverage, and intent-drift reviews. This
is the default. Require repro attempts for every HIGH/CRITICAL candidate. Use
the frontier reasoning/coding tier from `model-selection.md`.

## High assurance

Use only when explicitly requested or mandated. Run two independent passes and
a third only when the second adds a distinct MED+ candidate. Cap the profile at
three passes. Pin and record the exact model per pass. Union by root cause
rather than `(category, line)` alone, then
retain the strongest evidence and most precise location.

Independent workers are an optional venue capability. If delegation is
unavailable or unauthorized, perform the passes sequentially with a fresh work
list. Record the pass that surfaced each candidate, but never treat repeated
model agreement as confirmation; only source evidence or a fired reproducer
increases evidence strength.
