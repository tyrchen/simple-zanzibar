# Comprehensive Implementation Plan: Simplified Zanzibar

This plan details the phased implementation of the Simplified Zanzibar service, incorporating `pest` for DSL parsing.

## 1. Requirements Analysis
- **Goal**: Create a simplified, single-node, in-memory Rust implementation of Google's Zanzibar.
- **Core Features**:
    - Relation Tuples: `(object#relation@user)`
    - Namespace Configuration with `userset_rewrite` rules.
    - `check` API for authorization decisions.
    - `expand` API for understanding permissions.
    - DSL for defining policies, parsed using `pest`.
- **Scope Exclusions**:
    - Distributed system features (no replication, consensus).
    - Global consistency (no zookies, TrueTime).
    - Advanced performance optimizations.
    - Persistent storage (in-memory only, but extensible via `TupleStore` trait).

## 2. Components to be Developed
- **`src/model.rs`**: Core data structures (`Object`, `Relation`, `User`, `RelationTuple`, `NamespaceConfig`, etc.).
- **`src/store.rs`**: `TupleStore` trait and `InMemoryTupleStore` implementation.
- **`src/eval.rs`**: `check` and `expand` function implementations.
- **`src/parser.rs`**: DSL parsing logic using `pest`.
- **`src/grammar.pest`**: The formal grammar for the DSL.
- **`src/lib.rs`**: Main library entry point and public API.
- **`tests/`**: Unit and integration tests.
- **`examples/`**: Usage examples.

## 3. Architecture Considerations
- **Modularity**: Logic will be separated into `model`, `store`, `eval`, and `parser` modules.
- **Extensibility**: The `TupleStore` trait is designed to allow for future storage backends.
- **Error Handling**: Use `Result` and a custom `Error` enum (leveraging `thiserror`) for robust error handling.
- **API Design**: The public API in `lib.rs` will be idiomatic, well-documented Rust.

## 4. Implementation Strategy & Detailed Steps

### Phase 1: Foundation - Data Structures & Storage (`src/model.rs`, `src/store.rs`)
- [ ] **`model.rs`**:
    - Define `Object`, `Relation`, `User` (enum), `RelationTuple`.
    - Derive/implement `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`.
- [ ] **`store.rs`**:
    - Define `TupleStore` trait with `read_tuples`, `write_tuple`, `delete_tuple`.
    - Implement `InMemoryTupleStore` struct holding a `HashSet<RelationTuple>`.
    - Implement the `TupleStore` trait for `InMemoryTupleStore`.
- [ ] **`tests/storage_tests.rs`**:
    - Write unit tests to verify `InMemoryTupleStore` functionality.

### Phase 2: Core Evaluation Logic (`src/eval.rs`, `src/model.rs`)
- [ ] **`model.rs`**:
    - Define `UsersetExpression`, `RelationConfig`, `NamespaceConfig`.
    - Define `ExpandedUserset` enum for the `expand` API result.
- [ ] **`eval.rs`**:
    - Implement the recursive `check` function for all `UsersetExpression` variants.
    - Add cycle detection/recursion limit to `check`.
    - Implement the `expand` function, mirroring the `check` logic.
- [ ] **`tests/eval_tests.rs`**:
    - Write extensive unit tests for `check` and `expand`, covering all logical branches and cycle detection.

### Phase 3: DSL Parsing with `pest` (`src/parser.rs`, `src/grammar.pest`)
- [ ] **`src/grammar.pest`**:
    - Create the file and define the formal `pest` grammar for the policy language.
- [ ] **`Cargo.toml`**:
    - Confirm `pest` and add `pest_derive` if not already present.
- [ ] **`parser.rs`**:
    - Create a `ZanzibarParser` struct using `#[derive(Parser)]` and pointing to `grammar.pest`.
    - Implement a `parse_dsl` function that takes a string and returns `Result<Vec<NamespaceConfig>, Error>`.
    - Write logic to traverse the `pest` parse tree and construct the Rust data structures.
- [ ] **`tests/parser_tests.rs`**:
    - Write unit tests for the parser, testing both valid and invalid DSL syntax.

### Phase 4: Integration & API (`src/lib.rs`)
- [ ] **`lib.rs`**:
    - Organize all modules using `mod`.
    - Re-export all public-facing types and functions.
    - Add comprehensive documentation comments for the public API.
- [ ] **`tests/integration_tests.rs`**:
    - Create end-to-end tests that parse a DSL string, populate the store, and verify `check` and `expand` calls.
- [ ] **`examples/`**:
    - Create a simple example file (e.g., `examples/file_permissions.rs`) demonstrating a full usage cycle.

## 5. Challenges & Mitigations
- **Challenge**: The recursive logic in the evaluation engine can be complex.
    - **Mitigation**: Develop with extensive, focused unit tests for each logical branch.
- **Challenge**: Defining a correct and unambiguous `pest` grammar.
    - **Mitigation**: Develop the grammar iteratively, testing at each step.
- **Challenge**: Mapping the `pest` parse tree to Rust structs.
    - **Mitigation**: Write clean helper functions to transform specific parts of the tree.
