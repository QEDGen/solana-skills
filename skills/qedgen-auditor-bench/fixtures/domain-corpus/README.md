# Domain corpus fixtures

This compact, venue-neutral corpus exercises seven protocol domains that are not
captured by general structural patterns alone. Each `*-dossier.json` is a valid
v1 domain dossier containing one representative quantity and paired operation,
plus the smallest domain-specific candidate set needed by the expectation.

`expectations.json` is the benchmark oracle. It identifies the candidate
categories, units, paired operation, and intentionally unsupported language
concepts that an extractor/spec handoff should preserve.

Validate the corpus from this directory with:

```sh
./validate.sh
```
