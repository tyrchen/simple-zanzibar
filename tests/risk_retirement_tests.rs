//! Phase 0 risk-retirement tests for dependency API shape.

use std::{num::NonZeroU64, sync::Arc};

use arc_swap::ArcSwap;

#[derive(Debug, Eq, PartialEq)]
struct CompiledSchemaProbe {
    name: &'static str,
}

#[derive(Debug, Eq, PartialEq)]
struct RelationshipSnapshotProbe {
    tuple_count: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct PublishedSnapshotProbe {
    revision: NonZeroU64,
    schema_hash: [u8; 32],
    schema: Arc<CompiledSchemaProbe>,
    relationships: Arc<RelationshipSnapshotProbe>,
}

#[test]
fn test_should_publish_and_load_published_snapshot_shape_with_arc_swap() -> Result<(), &'static str>
{
    let initial_schema = Arc::new(CompiledSchemaProbe { name: "initial" });
    let initial_relationships = Arc::new(RelationshipSnapshotProbe { tuple_count: 0 });
    let initial = Arc::new(PublishedSnapshotProbe {
        revision: NonZeroU64::MIN,
        schema_hash: [1; 32],
        schema: Arc::clone(&initial_schema),
        relationships: Arc::clone(&initial_relationships),
    });
    let published = ArcSwap::from(Arc::clone(&initial));

    let loaded_initial = published.load_full();
    assert!(Arc::ptr_eq(&loaded_initial, &initial));
    assert!(Arc::ptr_eq(&loaded_initial.schema, &initial_schema));
    assert!(Arc::ptr_eq(
        &loaded_initial.relationships,
        &initial_relationships,
    ));

    let next_revision = NonZeroU64::new(2).ok_or("revision must be non-zero")?;
    let next_schema = Arc::new(CompiledSchemaProbe { name: "next" });
    let next_relationships = Arc::new(RelationshipSnapshotProbe { tuple_count: 1 });
    let next = Arc::new(PublishedSnapshotProbe {
        revision: next_revision,
        schema_hash: [2; 32],
        schema: Arc::clone(&next_schema),
        relationships: Arc::clone(&next_relationships),
    });
    published.store(Arc::clone(&next));

    let loaded_next = published.load_full();
    assert!(Arc::ptr_eq(&loaded_next, &next));
    assert!(Arc::ptr_eq(&loaded_next.schema, &next_schema));
    assert!(Arc::ptr_eq(&loaded_next.relationships, &next_relationships));
    assert_eq!(loaded_next.revision, next_revision);
    assert_eq!(loaded_next.schema_hash, [2; 32]);

    Ok(())
}
