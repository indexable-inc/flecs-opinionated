/* Run Flecs scripts in a world and read it back as DAG-CBOR (SPEC.md). Link the static or shared library built from
 * crates/flecs-snapshot. A world is used from one thread at a time. */
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

/* Each call returns 0 on success and 1 on failure. On success the result is in world_output; on failure the UTF-8
 * error ("script: line: message") is. The output stays valid until the next call on the same world. */
int32_t world_run(world *w, const uint8_t *name, size_t name_len, const uint8_t *code, size_t code_len);
/* roots: entity paths separated by '\n'. */
int32_t world_document(world *w, const uint8_t *roots, size_t roots_len); /* one DAG-CBOR document */
int32_t world_blocks(world *w, const uint8_t *roots, size_t roots_len);   /* a CARv1 file, root block first */
const uint8_t *world_output(const world *w, size_t *len);

#ifdef __cplusplus
}
#endif

#endif
