Bundled fonts
=============

- Inter (Regular, SemiBold) and JetBrains Mono (Regular), both under the SIL
  Open Font License 1.1 (see the LICENSE files next to this note).

The files are modified copies: `hack/strip-font-features.py` removed the
contextual-alternate and ligature features (`calt`, `liga`, `dlig`, `clig`,
`rlig`) so that text such as `gr4x4`, `->` or `--` is drawn exactly as
written. Nothing else was changed.
