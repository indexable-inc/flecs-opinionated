/* Run Flecs scripts in a world and read it back as DAG-CBOR (SPEC.md). Link the static or shared library built from
 * crates/flecs-snapshot with `--features ffi`. A world is used only on the thread that made it, and every world in a
 * process should be made on one thread (Flecs's log hook is process-wide). */
#ifndef WORLD_H
#define WORLD_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct world world;

/* A new world, or NULL if Flecs could not start one. */
world *world_new(void);
void world_free(world *w);

/* Each call returns 0 on success, 1 on failure, and -1 for a NULL world. On success the result is in world_output; on failure the UTF-8
 * error ("script: line: message") is. The output stays valid until the next call on the same world. */
int32_t world_run(world *w, const uint8_t *name, size_t name_len, const uint8_t *code, size_t code_len);
/* roots: entity paths separated by '\n'. */
int32_t world_document(world *w, const uint8_t *roots, size_t roots_len); /* one DAG-CBOR document */
int32_t world_blocks(world *w, const uint8_t *roots, size_t roots_len);   /* a CARv1 file, root block first */
int32_t world_ephemeral(world *w, const uint8_t *roots, size_t roots_len); /* Ephemeral parts, DAG-CBOR */
int32_t world_roots(world *w); /* the named root paths, '\n'-separated */
/* Sets a component on the entity at `path` to `value`, one DAG-CBOR value in the component's snapshot shape. */
int32_t world_set(world *w, const uint8_t *path, size_t path_len, const uint8_t *component, size_t component_len,
                  const uint8_t *value, size_t value_len);
/* The last call's output; NULL for a NULL world. `len` may be NULL or unaligned. */
const uint8_t *world_output(const world *w, size_t *len);

#ifdef __cplusplus
}
#endif

#endif
