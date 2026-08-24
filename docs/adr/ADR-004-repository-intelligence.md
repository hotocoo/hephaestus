# ADR-004: Repository Intelligence

## Status
Accepted

## Context
Agents need precise structural knowledge (symbols, references,
dependencies); LLM guesses are not acceptable when deterministic tools
can answer.

## Decision
Hybrid retrieval, deterministic-first:

1. Git facts via the system git binary invoked with argument vectors
   (never shell strings, never executing hooks from untrusted repos).
2. AST parsing via tree-sitter grammars (TS/JS, Python, Rust, Go, Java)
   behind a per-language extractor trait so languages plug in without
   core changes.
3. Symbol/reference index stored per snapshot in PostgreSQL.
4. Text search via Postgres full-text search; semantic embeddings are
   optional augmentation, never the source of truth.

All search results carry provenance: file, line range, symbol, and the
reason (e.g. referencing symbols). Fabricated locations are impossible
because every result originates from an indexed row.

## Consequences
* libgit2 parsing of hostile repositories is avoided; git CLI is a
  hardened, widely-fuzzed parser.
* tree-sitter grammar updates are pinned and tested against fixtures.
