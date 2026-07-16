# Release notes archive

This directory keeps shipped `RELEASE-v*.md` notes only. They are the detailed
changelog and migration record behind `references/release-history.md`.
Release notes describe behavior at that release and may mention flags or
schemas later changed or removed; use `README.md`, `SKILL.md`, and `references/`
for the current contract.

Planning drafts, PRDs, scoping notes, handoffs, spikes, manual audit notebooks,
and evaluation logs are intentionally ignored and disposable. Once work ships,
those files become stale quickly, duplicate the release notes and maintained
references, and can mislead readers with obsolete future-tense claims.

Use GitHub issues and pull requests for durable planning history. Record shipped
outcomes in the relevant maintained reference and release note; do not make
current code or documentation depend on an ignored planning file.
