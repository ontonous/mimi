#define _POSIX_C_SOURCE 200809L

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

enum {
    W4_SCOPE_LOOPBACK = 1,
    W4_ERR_SCOPE = -1001,
    W4_ERR_PORT = -1002,
    W4_ERR_SOCKET = -1003,
    W4_ERR_BIND = -1004,
    W4_ERR_LISTEN = -1005,
    W4_ERR_CONNECT = -1006,
    W4_ERR_FD = -1007,
};

enum {
    W4_EVENT_READY = 1,
    W4_EVENT_HANDOFF = 2,
    W4_EVENT_IN_FLIGHT = 3,
    W4_EVENT_RESPONSE = 4,
    W4_EVENT_BAD_JSON = 5,
    W4_EVENT_FFI_ERROR = 6,
    W4_EVENT_STOP_ACCEPTED = 7,
    W4_EVENT_WORKER_DONE = 8,
    W4_EVENT_DRAINED = 9,
};

/*
 * Closed endpoint capability: the only accepted tag denotes 127.0.0.1.
 * The helper never accepts a caller-provided address string or resolves DNS.
 * On success, the caller owns the returned fd and must close it once.
 */
int64_t w4_open_listener(int32_t scope, int32_t port, int32_t backlog) {
    if (scope != W4_SCOPE_LOOPBACK) {
        return W4_ERR_SCOPE;
    }
    if (port < 0 || port > 65535 || backlog < 1 || backlog > 64) {
        return W4_ERR_PORT;
    }

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return W4_ERR_SOCKET;
    }
    (void)fcntl(fd, F_SETFD, FD_CLOEXEC);

    int reuse = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (bind(fd, (const struct sockaddr *)&address, sizeof(address)) != 0) {
        close(fd);
        return W4_ERR_BIND;
    }
    if (listen(fd, backlog) != 0) {
        close(fd);
        return W4_ERR_LISTEN;
    }
    return (int64_t)fd;
}

/* S3 uses the same closed scope tag for the only outbound endpoint. */
int64_t w4_connect_loopback(int32_t scope, int32_t port) {
    if (scope != W4_SCOPE_LOOPBACK) {
        return W4_ERR_SCOPE;
    }
    if (port < 1 || port > 65535) {
        return W4_ERR_PORT;
    }

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return W4_ERR_SOCKET;
    }
    (void)fcntl(fd, F_SETFD, FD_CLOEXEC);

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0) {
        close(fd);
        return W4_ERR_CONNECT;
    }
    return (int64_t)fd;
}

int32_t w4_listener_port(int64_t raw_fd) {
    if (raw_fd < 0 || raw_fd > INT32_MAX) {
        return W4_ERR_FD;
    }
    int fd = (int)raw_fd;
    struct sockaddr_in address;
    socklen_t length = (socklen_t)sizeof(address);
    memset(&address, 0, sizeof(address));
    if (getsockname(fd, (struct sockaddr *)&address, &length) != 0) {
        return W4_ERR_FD;
    }
    if (address.sin_family != AF_INET ||
        address.sin_addr.s_addr != htonl(INADDR_LOOPBACK)) {
        return W4_ERR_SCOPE;
    }
    return (int32_t)ntohs(address.sin_port);
}

/* Frozen independent oracle: checksum(x) = x*x + 17 for 0 <= x <= 999. */
int32_t w4_checksum(int32_t value) {
    if (value < 0 || value > 999) {
        return -2;
    }
    return (int32_t)((int64_t)value * (int64_t)value + 17);
}

/* Fixed-size event records let the host schedule stop during in-flight work. */
int32_t w4_trace(int32_t code, int32_t detail) {
    char record[64];
    int length = snprintf(record, sizeof(record), "W4_EVENT %d %d\n", code, detail);
    if (length <= 0 || (size_t)length >= sizeof(record)) {
        return -1;
    }
    ssize_t written = write(STDERR_FILENO, record, (size_t)length);
    return written == length ? 0 : -1;
}

/* Negative ownership fixture only. Never call through a Mimi string import. */
const char *w4_borrowed_static_text(void) {
    return "C-owned static storage";
}
