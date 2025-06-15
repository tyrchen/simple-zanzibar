# System Patterns for Simplified Zanzibar

This document describes the key architectural and design patterns used in the implementation.

- **Decoupled Storage**: The `TupleStore` trait abstracts the storage layer (e.g., in-memory `HashSet`) from the core authorization logic. This allows for future extension with different storage backends (like file-based or SQLite) without refactoring the evaluation engine.

- **Recursive Policy Evaluation**: The `check` and `expand` functions are fundamentally recursive. They traverse the `UsersetExpression` tree defined in a `NamespaceConfig` to resolve permissions. This pattern is central to how Zanzibar handles complex and nested policies like "editors are also viewers."

- **Recursive Descent Parsing**: The DSL will be parsed into Rust data structures. A recursive descent parser is a suitable approach for this, where each function in the parser corresponds to a rule in the DSL grammar. Alternatively, a parser-combinator library like `nom` or `pest` could be used.

- **Type-Safe Modeling**: The project leverages Rust's strong type system, particularly structs and enums, to create a precise and type-safe representation of Zanzibar's data model (`Object`, `Relation`, `User`, `UsersetExpression`, etc.). This prevents invalid states at compile time.

- **Cycle Detection**: The recursive evaluation logic must include mechanisms to handle cyclic dependencies in policies (e.g., group A is a member of group B, and B is a member of A). This will be implemented by tracking the evaluation path (e.g., with a `HashSet` of visited nodes) or by enforcing a maximum recursion depth to prevent stack overflows.
