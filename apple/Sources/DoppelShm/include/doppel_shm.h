// What Swift can't call directly: shm_open is variadic, and 64-bit
// release/acquire stores and loads need macOS 15's Synchronization module.
#ifndef DOPPEL_SHM_H
#define DOPPEL_SHM_H

#include <stddef.h>
#include <stdint.h>

/// An anonymous shared memory object of `length` bytes: created under a
/// random name, then unlinked, so only descriptors reach it. -1 and errno
/// on failure.
int doppel_shm_create(size_t length);

void doppel_store_release_u64(void *address, uint64_t value);

#endif
