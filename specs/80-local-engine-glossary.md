# 80 - Local Engine Glossary

Status: draft v1
Owner: Simple Zanzibar

## Relationship

A stored authorization fact in the form `resource#relation@subject`. In legacy code this was `RelationTuple`; v2 uses `Relationship`.

## Resource

The object being protected, represented by `ObjectRef`.

## Subject

The user or userset receiving access. A direct subject is `user:alice`; a userset subject is `group:eng#member`.

## Relation

A stored edge name on a resource, such as `owner`, `parent`, or `member`.

## Permission

A computed relation defined by schema expression. In v2, permissions and relations share `RelationName` but schema definitions distinguish stored relations from computed permissions.

## Userset

A subject that points to another `object#relation`.

## Tuple-To-Userset

An expression that follows a relation from the current resource to intermediate objects, then checks another relation on those objects.

## Snapshot

An immutable pair of schema and relationship indexes at one revision.

## Revision

A monotonic local version assigned after each successful schema or relationship write.

## Consistency Token

External string carrying revision, schema hash, and datastore ID. It lets callers ask for exact snapshot reads.

## Membership

Internal result of graph evaluation. Public `check` maps it to allowed/denied, while the internal enum reserves conditional state for future caveats.

## Cross-References

- Related specs: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
