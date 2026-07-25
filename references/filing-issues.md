# Filing a qedgen issue: the fail-fast procedure

Load this file when SKILL.md's fail-fast rule fires — that is, when `qedgen
check`, `qedgen codegen`, a `cargo` build of the generated crate, `cargo kani`,
`cargo test`, or `lake build` on generated output emits an error you have no
documented path past.

The trigger conditions and the anti-pattern list live in SKILL.md, because an
agent has to recognize the situation before it knows to load this file. What
follows is the procedure.

## Why this is a codegen bug

In every triggering case the failure is a **codegen bug**, not a spec bug — the
user wrote a valid spec and the generator emitted broken output. Hand-editing
the generated file (`guards.rs`, `state.rs`, `lib.rs`'s Accounts structs,
`Spec.lean`, `kani.rs`, `proptest.rs`, the `imported/` mirror) is the worst
possible response: the next `qedgen codegen` regenerates over your edit and the
fix evaporates.

## The fail-fast script

1. **Surface the error verbatim** *to the user, in your reply* — not yet to
   GitHub. Give the exact lint name (`unsupported_quantifier_shape`,
   `no_access_control`, `missing_cpi_for_token_context`, etc.) or the exact
   compiler / Kani / Lake error message, plus the spec fragment and (for
   codegen-output failures) the generated line that tripped it.

2. **Check `docs/limitations.md`** — many shapes already have documented status
   (deferred, workaround, won't-fix). If your shape is listed and the documented
   workaround doesn't lie about the spec, follow it.

3. **Construct a sanitized minimal reproducer.** Before drafting any issue body,
   REWRITE the failing fragment as a *generic* repro. The bug is in qedgen's
   handling of a shape, not in the user's specific business logic — the issue
   only needs the shape. **Get explicit user approval before sending any
   reproducer to GitHub** (step 4).

   **MUST scrub:**
   - Real pubkeys / addresses (use `"11111111111111111111111111111111"`
     placeholder or omit `program_id` from the reproducer entirely)
   - Token mint addresses, oracle keys, treasury / multisig pubkeys
   - Named accounts, fields, handlers, and error variants that hint at protocol
     identity (anything that ties the spec to a specific product name, brand, or
     recognizable on-chain protocol) — rename to generic shapes (`admin`,
     `vault`, `pool`, `Foo`, `Bar`, `Action`)
   - Constants encoding deal-specific numbers (fee bps, tier thresholds,
     hardcoded ratios) — replace with `0` / `1` / a generic literal
   - Account schema fields that aren't load-bearing for the bug — drop them
     entirely; keep only the fields the failing construct touches
   - Comments that reveal product names, customer names, internal team handles,
     deadlines, audit findings, or competitive context
   - File paths under `programs/` / `formal_verification/` — strip directory
     prefixes; refer to files by their generated role (`guards.rs`, `state.rs`,
     the handler scaffold for `<handler_name>`)
   - The repo path in any stack trace (sed out `/Users/<name>/code/<project>/`)

   **MAY include:**
   - The exact lint name / compiler error name / lake elaboration message (these
     are qedgen-side identifiers, not user data)
   - The generic spec shape (`type State | A of {…} | B of {…}` with anonymized
     field names) that triggers the failing path
   - The generated-file role (`guards.rs`'s emitted check for the handler) —
     without the real handler name

4. **Ask the user before filing.** Once you have a sanitized reproducer, show it
   to the user and ask: "Is this safe to file at
   github.com/qedgen/solana-skills/issues? It will be public." Default answer is
   no — many specs describe pre-launch or closed-source programs whose
   architecture leaking is real harm. If the user declines OR doesn't respond,
   do NOT file. Hand them the sanitized reproducer to file themselves when
   they're ready.

5. **If the user authorizes**, file the issue:
   ```bash
   gh issue create \
     --title "qedgen: <generic one-line summary, no product names>" \
     --body "$(cat <<EOF
   ## Sanitized spec fragment

   \`\`\`fsharp
   <minimal generic reproducer — 10-20 lines, no real pubkeys / business logic / product names>
   \`\`\`

   ## Shape this is meant to cover

   <one paragraph: the GENERIC pattern that's tripping qedgen, not what the user is building>

   ## What qedgen / cargo / kani / lake says

   \`\`\`
   <verbatim error output — strip user-paths / personal info, keep qedgen-side identifiers>
   \`\`\`

   ## Generated file role (if a codegen-output failure)

   <which generated file (guards.rs / state.rs / Spec.lean / kani.rs / etc.) the codegen-emitted line lives in, plus the offending lines AFTER scrubbing>

   ## Workarounds considered and rejected

   <list anti-patterns considered + why each lies about the spec shape>
   EOF
   )"
   ```

6. **Then pause and tell the user.** Don't auto-apply a workaround "for now."
   Auto-workarounds — in the spec OR in the generated output — are how
   `phantom_admin: Pubkey` ends up in production state forever (the
   friction-report's #6 was exactly this shape) and how generated Anchor crates
   accumulate hand-edited drift that regen overwrites.

## The workaround exception

If the user explicitly tells you to ship a workaround (with phrasing like "just
inline it for now" / "phantom field is fine" / "we'll fix it later" /
"hand-edit the guard for this PR"), apply the workaround AND leave a
`// FIXME(qedgen-issue: <url>):` comment pointing at the issue. The marker makes
the regression auditable later. For hand-edits to generated files, ALSO note in
the issue body that the edit will be overwritten on the next `qedgen codegen` —
the user needs to know.

The exception does NOT cover filing issues: even when the user authorizes a
workaround, the issue body must still be sanitized per step 3. The workaround
marker is private (lives in their repo); the issue is public.
