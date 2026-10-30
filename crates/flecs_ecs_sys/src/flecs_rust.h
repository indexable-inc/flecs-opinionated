#include "flecs.h"

/* flecs_ecs reads component pointers through ecs_get_ptr_t. Flecs-Rust's patched Flecs C made it a struct with lock
 * information under FLECS_MUT_ALIAS_LOCKS (its `flecs_safety_locks` feature); upstream Flecs has no such locks, so here
 * it is always the bare pointer, and flecs_record_get_id and flecs_record_get_mut_id are defined in flecs_rust.c on
 * upstream's internals instead of in flecs.c. */
typedef void* ecs_get_ptr_t;

/* ecs_get_id() for an entity whose record the caller already has: inherited (IsA) components included. */
FLECS_API
ecs_get_ptr_t flecs_record_get_id(
    const ecs_world_t *world,
    ecs_entity_t entity,
    const ecs_record_t *r,
    ecs_id_t component);

/* ecs_get_mut_id() for an entity whose record the caller already has: owned components only. */
FLECS_API
ecs_get_ptr_t flecs_record_get_mut_id(
    const ecs_world_t *world,
    const ecs_record_t *r,
    ecs_id_t component);

/* ecs_record_find() of Flecs 4.1.6, which upstream master removed (its replacement flecs_entities_get asserts that the
 * entity is alive and takes no stage): the entity's record, or NULL when the entity is not alive. Accepts a
 * stage. */
FLECS_API
ecs_record_t* ecs_rust_record_find(
    const ecs_world_t *world,
    ecs_entity_t entity);

typedef struct ecs_rust_set_t {
    void *ptr;
    bool call_modified;
    bool is_new;
} ecs_rust_set_t;

FLECS_API
    int32_t ecs_rust_rel_count(
    const ecs_world_t *world,
    ecs_id_t id,
    ecs_table_t* table);

FLECS_API
ecs_entity_t ecs_rust_get_typeid(
    const ecs_world_t *world,
    ecs_id_t id,
    const ecs_component_record_t* idr);

FLECS_API
const ecs_type_info_t* ecs_rust_get_type_info_from_record(
    const ecs_world_t *world,
    ecs_id_t id,
    const ecs_component_record_t* idr);

FLECS_API
ecs_rust_set_t ecs_rust_set(
    ecs_world_t *world,
    ecs_entity_t entity,
    ecs_id_t id,
    const void *new_ptr,
    size_t size);

/* Fast path for compile-time-known sparse / dont_fragment components without
 * the (OnInstantiate, Inherit) trait. ecs_get_sparse_id() as an ecs_get_ptr_t. */
FLECS_API
ecs_get_ptr_t ecs_rust_get_sparse_id(
    const ecs_world_t *world,
    ecs_entity_t entity,
    ecs_id_t id,
    size_t size);

/* ABI guards: C-side sizeof for structs with FLECS_DEBUG-gated fields, so the
 * Rust side can assert its bindings match the compiled profile. */
FLECS_API size_t ecs_rust_sizeof_ecs_ref_t(void);
FLECS_API size_t ecs_rust_sizeof_ecs_map_t(void);
FLECS_API size_t ecs_rust_sizeof_ecs_map_iter_t(void);
FLECS_API size_t ecs_rust_sizeof_ecs_stack_t(void);
FLECS_API size_t ecs_rust_sizeof_ecs_stack_cursor_t(void);
