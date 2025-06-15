# Active Context for Simplified Zanzibar Project

This document provides a summary of the project's context, derived from the full memory bank.

## Project Brief
The goal is to build a simplified, single-node, in-memory authorization system in Rust, inspired by Google's Zanzibar. It focuses on implementing the core logic (relation tuples, policy evaluation) while omitting distributed system complexities.

## Product Context
The system is an authorization engine for local development and small-scale applications. It provides `check` and `expand` APIs for permission queries, with policies defined in a custom DSL.

## Technical Context
- **Data Model**: Rust structs and enums (`Object`, `Relation`, `User`, `RelationTuple`, `NamespaceConfig`, `UsersetExpression`).
- **API**: `TupleStore` trait for storage, `check` function for boolean authorization, `expand` function for listing permissions.
- **DSL**: A text-based language for defining authorization policies.

## System Patterns
- Decoupled storage via `TupleStore` trait.
- Recursive evaluation of `UsersetExpression` policy trees.
- Cycle detection during evaluation.
- Type-safe modeling using Rust's type system.

Refer to `tasks.md` for the detailed implementation plan.
