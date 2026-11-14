#define _POSIX_C_SOURCE 200809L

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

enum { W4_SCOPE_LOOPBACK = 1, W4_ERR_SCOPE = -1001, W4_ERR_PORT = -1002 };
extern int64_t w4_open_listener_s3(int32_t, int32_t, int32_t);
extern int64_t w4_connect_loopback_s3(int32_t, int32_t);
extern int32_t w4_listener_port(int64_t);

static unsigned socket_calls;
static unsigned bind_calls;
static unsigned connect_calls;
static unsigned listen_calls;

extern int __real_socket(int, int, int);
extern int __real_bind(int, const struct sockaddr *, socklen_t);
extern int __real_connect(int, const struct sockaddr *, socklen_t);
extern int __real_listen(int, int);

int __wrap_socket(int domain, int type, int protocol) {
    ++socket_calls;
    return __real_socket(domain, type, protocol);
}

int __wrap_bind(int fd, const struct sockaddr *address, socklen_t length) {
    ++bind_calls;
    return __real_bind(fd, address, length);
}

int __wrap_connect(int fd, const struct sockaddr *address, socklen_t length) {
    ++connect_calls;
    return __real_connect(fd, address, length);
}

int __wrap_listen(int fd, int backlog) {
    ++listen_calls;
    return __real_listen(fd, backlog);
}

int main(void) {
    assert(w4_open_listener_s3(0, 0, 8) == W4_ERR_SCOPE);
    assert(w4_open_listener_s3(W4_SCOPE_LOOPBACK, 23999, 8) == W4_ERR_PORT);
    assert(w4_connect_loopback_s3(0, 24000) == W4_ERR_SCOPE);
    assert(w4_connect_loopback_s3(W4_SCOPE_LOOPBACK, 24101) == W4_ERR_PORT);
    assert(socket_calls == 0 && bind_calls == 0 &&
           connect_calls == 0 && listen_calls == 0);
    int64_t listener = w4_open_listener_s3(W4_SCOPE_LOOPBACK, 0, 8);
    assert(listener >= 0);
    int32_t selected_port = w4_listener_port(listener);
    assert(selected_port >= 24000 && selected_port <= 24100);
    assert(socket_calls == 1 && bind_calls == 1 &&
           connect_calls == 0 && listen_calls == 1);
    assert(close((int)listener) == 0);
    puts("S3_SCOPE_ORACLE_OK denied_syscalls=0 allowed_listener_syscalls=1");
    return 0;
}
