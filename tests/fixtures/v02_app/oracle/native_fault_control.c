/*
 * Deliberate detector controls for A-11.  This file is never linked into a
 * release Mimi artifact.  `--uaf` must be rejected by AddressSanitizer and
 * `--race` must be rejected by ThreadSanitizer; a clean exit is a detector
 * failure, not a pass.
 */
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

static volatile int64_t race_counter;

__attribute__((noinline)) static int touch_after_free(char *pointer) {
    volatile char *probe = pointer;
    probe[0] = 1;
    return probe[0];
}

static void *race_worker(void *arg) {
    (void)arg;
    for (int i = 0; i < 100000; ++i) {
        race_counter += 1;
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (strcmp(argv[1], "--uaf") == 0) {
        char *p = (char *)malloc(8);
        if (!p) return 3;
        memset(p, 0x5a, 8);
        free(p);
        /* Intentional use-after-free detector control. */
        return touch_after_free(p) == 1 ? 0 : 4;
    }
    if (strcmp(argv[1], "--race") == 0) {
        pthread_t a, b;
        if (pthread_create(&a, NULL, race_worker, NULL) != 0) return 5;
        if (pthread_create(&b, NULL, race_worker, NULL) != 0) return 6;
        pthread_join(a, NULL);
        pthread_join(b, NULL);
        return race_counter == 200000 ? 0 : 7;
    }
    return 8;
}
