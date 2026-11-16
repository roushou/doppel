#include "doppel_shm.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

int doppel_shm_create(size_t length) {
    // Names are at most PSHMNAMLEN (31) bytes.
    char name[32];
    for (int attempt = 0; attempt < 8; attempt++) {
        snprintf(name, sizeof name, "/doppel.%08x%08x", arc4random(), arc4random());
        int fd = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
        if (fd < 0) {
            if (errno == EEXIST) continue;
            return -1;
        }
        shm_unlink(name);
        if (ftruncate(fd, (off_t)length) != 0) {
            int saved = errno;
            close(fd);
            errno = saved;
            return -1;
        }
        return fd;
    }
    errno = EEXIST;
    return -1;
}

void doppel_store_release_u64(void *address, uint64_t value) {
    __atomic_store_n((uint64_t *)address, value, __ATOMIC_RELEASE);
}
