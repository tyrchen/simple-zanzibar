# Specs Index

Status: draft v1
Last updated: 2026-05-23

This directory now has two generations of design material:

- `0001-design.md`: legacy paper-era Simple Zanzibar design. It remains useful as background for the original toy implementation.
- The numbered v2 spec set below: the build contract for rebuilding Simple Zanzibar into a high performance local Rust Zanzibar engine library.

Read the v2 specs in numeric order. The order is also the implementation dependency order unless a cross-cut explicitly says otherwise.

## Spec Table

| File | Type | Purpose |
| --- | --- | --- |
| [00-local-engine-prd.md](./00-local-engine-prd.md) | PRD | Product goals, users, non-goals, success metrics, naming rules. |
| [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md) | Design | Domain types, validation boundaries, serialized shapes, relationship grammar. |
| [11-schema-system-design.md](./11-schema-system-design.md) | Design | Schema parser/compiler/type-system resolver and schema application rules. |
| [12-relationship-store-design.md](./12-relationship-store-design.md) | Design | Indexed in-memory relationship store, query filters, write mutations, preconditions. |
| [13-revision-consistency-design.md](./13-revision-consistency-design.md) | Design | Local revision tokens, snapshot readers, schema hashes, copy-on-write publication. |
| [14-evaluation-engine-design.md](./14-evaluation-engine-design.md) | Design | Check, expand, lookup, membership algebra, recursion policy, execution context. |
| [15-public-api-design.md](./15-public-api-design.md) | Design | Crate-facing API, compatibility facade, error mapping, examples. |
| [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md) | Design | Compact row storage, identifier interning, `Vec<RowId>` postings, snapshot ownership cleanup, memory targets. |
| [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md) | Design | Versioned compact snapshot artifact for fast load, bounded load-time RSS, and practical disk size. |
| [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md) | Design | Trusted `.szsnap` v2 load mode, serialized symbol hashes/lookups, and <= 200 ms 1M-rule cold-load path. |
| [60-crates-features-design.md](./60-crates-features-design.md) | Design | Crate layout, feature flags, dependency policy, current crate-version survey. |
| [70-security-design.md](./70-security-design.md) | Design | Threat model, validation limits, panic policy, unsafe policy, logging/data exposure. |
| [71-performance-budgets-design.md](./71-performance-budgets-design.md) | Design | Performance targets, benchmark matrix, profiling rules, CI gates. |
| [72-testing-verification-plan.md](./72-testing-verification-plan.md) | Verification plan | Unit, integration, property, compatibility, and benchmark verification. |
| [80-local-engine-glossary.md](./80-local-engine-glossary.md) | Glossary | Terms whose meaning must stay stable across specs and code. |
| [90-local-engine-roadmap.md](./90-local-engine-roadmap.md) | Roadmap | Stakeholder-facing milestones and observable exit criteria. |
| [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md) | Implementation plan | Engineer-facing, dependency-ordered phases with task gates. |
| [93-improvements-review.md](./93-improvements-review.md) | Review backlog | Deferred review findings and out-of-phase follow-up implementation items. |
| [99-key-decisions.md](./99-key-decisions.md) | Decisions | Load-bearing design choices, alternatives, and rationale. |

## Build-Order Graph

```text
                         +-------------------------+
                         | 00 PRD                  |
                         | local engine goals      |
                         +------------+------------+
                                      |
                                      v
                         +-------------------------+
                         | 10 Data Model           |
                         | typed IDs and tuples    |
                         +------------+------------+
                                      |
        +-----------------------------+-----------------------------+
        |                             |                             |
        v                             v                             v
+-------------------+       +---------------------+       +----------------------+
| 11 Schema System  | ----> | 12 Relationship     | ----> | 13 Revision and      |
| parse/compile/    |       | Store               |       | Consistency          |
| type-check        |       | indexed snapshots   |       | tokens/snapshots     |
+---------+---------+       +----------+----------+       +----------+-----------+
          |                            |                             |
          +----------------------------+-------------+---------------+
                                                       |
                                                       v
                                            +----------------------+
                                            | 14 Evaluation Engine |
                                            | check/expand/lookup  |
                                            +----------+-----------+
                                                       |
                                                       v
                                            +----------------------+
                                            | 15 Public API        |
                                            | facade/examples      |
                                            +----------+-----------+
                                                       |
                                                       v
                                            +----------------------+
                                            | 16 Compact Store     |
                                            | memory efficiency    |
                                            +----------+-----------+
                                                       |
                                                       v
                                            +----------------------+
                                            | 17 Snapshot Format   |
                                            | fast cold load       |
                                            +----------+-----------+
                                                       |
                                                       v
                                            +----------------------+
                                            | 18 Trusted Fast Load |
                                            | <=200ms cold start   |
                                            +----------+-----------+
                                                       |
                +------------------------------+-------+------------------------------+
                |                              |                                      |
                v                              v                                      v
       +----------------+            +-------------------+                  +------------------+
       | 60 Crates and  |            | 70 Security       |                  | 71 Performance   |
       | Features       |            |                   |                  | Budgets          |
       +-------+--------+            +---------+---------+                  +---------+--------+
               |                               |                                      |
               +-------------------------------+------------------+-------------------+
                                                                  |
                                                                  v
                                                       +----------------------+
                                                       | 72 Verification      |
                                                       | Plan                 |
                                                       +----------+-----------+
                                                                  |
                                                                  v
                  +-------------------------+          +----------------------+
                  | 90 Roadmap              | <------> | 91 Impl Plan         |
                  | stakeholder milestones  |          | engineering phases   |
                  +-------------------------+          +----------------------+
```

## Required Prior Art

- SpiceDB research memo: [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md)
- Vendored SpiceDB source: `vendors/spicedb` pinned at `9a71382960c2912f8debeaaeb98ae9288cb3f092`
- Legacy Simple Zanzibar design: [0001-design.md](./0001-design.md)
