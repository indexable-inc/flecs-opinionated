/* The things Rust would otherwise have to mirror flecs's large structs for: setting the OS
 * API hooks (ecs_os_api_t), listing children and roots (ecs_iter_t, ecs_query_desc_t), and the sizes of the structs Rust does
 * mirror, for a test that compares them on every target. */
#include <stddef.h>
#include "flecs.h"

typedef void (*glue_log_t)(int32_t level, const char *file, int32_t line, const char *msg);

static void glue_zero_time(ecs_time_t *t) {
    t->sec = 0;
    t->nanosec = 0;
}

static uint64_t glue_zero_now(void) { return 0; }

static void glue_sleep(int32_t sec, int32_t nanosec) {
    (void)sec;
    (void)nanosec;
}

/* Flecs's defaults (its own malloc wrappers over the C library), with logging sent to `log`,
 * abort sent to `abort_fn`, and every clock reading 0: nothing here may depend on the time, so
 * the same scripts give the same world everywhere. Call before the first ecs_init. */
void glue_os_api(glue_log_t log, void (*abort_fn)(void)) {
    ecs_os_set_api_defaults();
    ecs_os_api_t api = ecs_os_get_api();
    api.log_ = log;
    api.abort_ = abort_fn;
    api.get_time_ = glue_zero_time;
    api.now_ = glue_zero_now;
    api.sleep_ = glue_sleep;
    api.flags_ &= ~(ecs_flags32_t)EcsOsApiLogWithColors;
    ecs_os_set_api(&api);
}

/* The children of `parent` (ChildOf, either storage), in flecs's order: the declared order when
 * the parent has OrderedChildren, otherwise table order. Writes up to `cap` ids to `out` and
 * returns how many there are, so a caller with too small a buffer calls again. */
int32_t glue_children(const ecs_world_t *world, ecs_entity_t parent, ecs_entity_t *out, int32_t cap) {
    int32_t n = 0;
    ecs_iter_t it = ecs_children(world, parent);
    while (ecs_children_next(&it)) {
        for (int32_t i = 0; i < it.count; i++) {
            if (n < cap) out[n] = it.entities[i];
            n++;
        }
    }
    return n;
}

/* Every named entity without a parent, prefabs and disabled entities included (a raw walk of the
 * tables with a name, so no query features are needed). Same contract as glue_children. */
int32_t glue_roots(ecs_world_t *world, ecs_entity_t *out, int32_t cap) {
    int32_t n = 0;
    ecs_iter_t it = ecs_each_id(world, ecs_pair(ecs_id(EcsIdentifier), EcsName));
    while (ecs_each_next(&it)) {
        for (int32_t i = 0; i < it.count; i++) {
            if (ecs_get_parent(world, it.entities[i])) continue;
            if (n < cap) out[n] = it.entities[i];
            n++;
        }
    }
    return n;
}

/* sizeof and alignof of the structs crates/flecs-c/src/lib.rs mirrors, by index. */
size_t glue_layout(int32_t which, int32_t align) {
#define L(T) return align ? _Alignof(T) : sizeof(T)
    switch (which) {
    case 0: L(ecs_type_t);
    case 1: L(EcsType);
    case 2: L(EcsPrimitive);
    case 3: L(ecs_vec_t);
    case 4: L(EcsStruct);
    case 5: L(ecs_member_t);
    case 6: L(EcsEnum);
    case 7: L(ecs_enum_constant_t);
    case 8: L(ecs_bitmask_constant_t);
    case 9: L(EcsConstants);
    case 10: L(EcsArray);
    case 11: L(EcsVector);
    case 12: L(EcsComponent);
    default: return 0;
    }
#undef L
}

/* offsetof of the ecs_member_t fields Rust reads, by index. */
size_t glue_member_offset(int32_t which) {
    switch (which) {
    case 0: return offsetof(ecs_member_t, name);
    case 1: return offsetof(ecs_member_t, type);
    case 2: return offsetof(ecs_member_t, count);
    case 3: return offsetof(ecs_member_t, offset);
    case 4: return offsetof(ecs_member_t, size);
    default: return 0;
    }
}
