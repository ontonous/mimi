#define _POSIX_C_SOURCE 200809L

#include <stdint.h>

enum { W4_SCOPE_LOOPBACK = 1, W4_ERR_SCOPE = -1001, W4_ERR_PORT = -1002 };

extern int64_t w4_open_listener(int32_t scope, int32_t port, int32_t backlog);
extern int64_t w4_connect_loopback(int32_t scope, int32_t port);
extern int32_t w4_listener_port(int64_t fd);

/* S3 narrows the ordinary listener and connector range before OS calls. */
int64_t w4_open_listener_s3(int32_t scope, int32_t port, int32_t backlog) {
    if (scope != W4_SCOPE_LOOPBACK) {
        return W4_ERR_SCOPE;
    }
    if (port != 0 && (port < 24000 || port > 24100)) {
        return W4_ERR_PORT;
    }
    if (port == 0) {
        for (int32_t candidate = 24000; candidate <= 24100; ++candidate) {
            int64_t fd = w4_open_listener(scope, candidate, backlog);
            if (fd >= 0) {
                return fd;
            }
            if (fd != -1004) {
                return fd;
            }
        }
        return -1004;
    }
    return w4_open_listener(scope, port, backlog);
}

int64_t w4_connect_loopback_s3(int32_t scope, int32_t port) {
    if (scope != W4_SCOPE_LOOPBACK) {
        return W4_ERR_SCOPE;
    }
    if (port < 24000 || port > 24100) {
        return W4_ERR_PORT;
    }
    return w4_connect_loopback(scope, port);
}
