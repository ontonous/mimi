#define _POSIX_C_SOURCE 200809L

#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <arpa/inet.h>
#include <netinet/in.h>

enum {
    W4_SCOPE_LOOPBACK = 1,
    W4_ERR_SCOPE = -1001,
    W4_ERR_PORT = -1002,
    W4_EVENT_READY = 1,
    W4_EVENT_HANDOFF = 2,
    W4_EVENT_IN_FLIGHT = 3,
    W4_EVENT_RESPONSE = 4,
    W4_EVENT_BAD_JSON = 5,
    W4_EVENT_FFI_ERROR = 6,
    W4_EVENT_STOP_ACCEPTED = 7,
    W4_EVENT_WORKER_DONE = 8,
    W4_EVENT_DRAINED = 9,
    EVENT_LIMIT = 8192,
    IO_TIMEOUT_MS = 8000,
};

typedef int32_t (*checksum_fn)(int32_t);
typedef int64_t (*open_listener_fn)(int32_t, int32_t, int32_t);
typedef int32_t (*listener_port_fn)(int64_t);
typedef int32_t (*factorial_fn)(int32_t);
typedef int32_t (*status_fn)(int32_t);

typedef struct {
    int code;
    int detail;
    struct timespec seen_at;
} Event;

typedef struct {
    int port;
    int soak_count;
    int role;
    int result;
    char failure[256];
    pthread_barrier_t *start_barrier;
} Client;

typedef struct {
    pid_t pid;
    atomic_int stop;
    atomic_int max_fds;
    atomic_int max_threads;
} ProcessMetrics;

static Event events[EVENT_LIMIT];
static size_t event_count;

static uint64_t monotonic_ms(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000U + (uint64_t)now.tv_nsec / 1000000U;
}

static void fail_message(char *out, size_t capacity, const char *message) {
    if (capacity == 0) {
        return;
    }
    (void)snprintf(out, capacity, "%s", message);
}

static int load_symbol(void *library, const char *name, void *function_out,
                       size_t function_size) {
    void *symbol = dlsym(library, name);
    if (symbol == NULL || function_size != sizeof(symbol)) {
        return -1;
    }
    memcpy(function_out, &symbol, function_size);
    return 0;
}

#define LOAD_SYMBOL(handle, name, function) \
    do { \
        if (load_symbol((handle), (name), &(function), sizeof(function)) != 0) { \
            fprintf(stderr, "missing/incompatible symbol: %s\n", (name)); \
            return -1; \
        } \
    } while (0)

static int check_c_oracle(const char *trusted_path, const char *exports_path) {
    void *trusted = dlopen(trusted_path, RTLD_NOW | RTLD_LOCAL);
    if (trusted == NULL) {
        fprintf(stderr, "dlopen trusted C library: %s\n", dlerror());
        return -1;
    }
    void *exports = dlopen(exports_path, RTLD_NOW | RTLD_LOCAL);
    if (exports == NULL) {
        fprintf(stderr, "dlopen Mimi export library: %s\n", dlerror());
        (void)dlclose(trusted);
        return -1;
    }

    checksum_fn checksum = NULL;
    open_listener_fn open_listener = NULL;
    listener_port_fn listener_port = NULL;
    factorial_fn factorial = NULL;
    status_fn status = NULL;
    LOAD_SYMBOL(trusted, "w4_checksum", checksum);
    LOAD_SYMBOL(trusted, "w4_open_listener", open_listener);
    LOAD_SYMBOL(trusted, "w4_listener_port", listener_port);
    LOAD_SYMBOL(exports, "w4_factorial", factorial);
    LOAD_SYMBOL(exports, "w4_status", status);

    const int32_t inputs[] = {0, 20, 21, 999, 1000, -1};
    const int32_t expected[] = {17, 417, 458, 998018, -2, -2};
    for (size_t index = 0; index < sizeof(inputs) / sizeof(inputs[0]); ++index) {
        int32_t observed = checksum(inputs[index]);
        if (observed != expected[index]) {
            fprintf(stderr, "C checksum oracle mismatch: input=%" PRId32
                            " expected=%" PRId32 " observed=%" PRId32 "\n",
                    inputs[index], expected[index], observed);
            (void)dlclose(exports);
            (void)dlclose(trusted);
            return -1;
        }
    }

    if (open_listener(0, 0, 8) != W4_ERR_SCOPE ||
        open_listener(W4_SCOPE_LOOPBACK, -1, 8) != W4_ERR_PORT) {
        fprintf(stderr, "endpoint scope/port denial oracle mismatch\n");
        (void)dlclose(exports);
        (void)dlclose(trusted);
        return -1;
    }
    int64_t listener = open_listener(W4_SCOPE_LOOPBACK, 0, 8);
    if (listener < 0) {
        fprintf(stderr, "trusted C loopback listener creation failed: %" PRId64 "\n",
                listener);
        (void)dlclose(exports);
        (void)dlclose(trusted);
        return -1;
    }
    int32_t port = listener_port(listener);
    struct sockaddr_in address;
    socklen_t address_length = (socklen_t)sizeof(address);
    memset(&address, 0, sizeof(address));
    int address_ok = getsockname((int)listener, (struct sockaddr *)&address,
                                 &address_length) == 0 &&
                     ntohl(address.sin_addr.s_addr) == INADDR_LOOPBACK;
    (void)close((int)listener);
    if (port < 1 || port > 65535 || !address_ok) {
        fprintf(stderr, "loopback binding oracle mismatch\n");
        (void)dlclose(exports);
        (void)dlclose(trusted);
        return -1;
    }

    if (factorial(5) != 120 || status(5) != 121 ||
        dlsym(exports, "w4_checksum") != NULL) {
        fprintf(stderr, "Mimi C-export wrapper/recursion/symbol-boundary mismatch\n");
        (void)dlclose(exports);
        (void)dlclose(trusted);
        return -1;
    }

    printf("C_ORACLE_OK vectors=6 loopback=127.0.0.1 exports=factorial,status\n");
    (void)dlclose(exports);
    (void)dlclose(trusted);
    return 0;
}

static int set_socket_timeouts(int fd) {
    struct timeval timeout;
    timeout.tv_sec = IO_TIMEOUT_MS / 1000;
    timeout.tv_usec = (IO_TIMEOUT_MS % 1000) * 1000;
    return setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) == 0 &&
           setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout)) == 0
               ? 0
               : -1;
}

static int connect_loopback(int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0 || set_socket_timeouts(fd) != 0) {
        if (fd >= 0) {
            (void)close(fd);
        }
        return -1;
    }
    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0) {
        (void)close(fd);
        return -1;
    }
    return fd;
}

static int send_all(int fd, const char *text) {
    size_t length = strlen(text);
    size_t sent = 0;
    while (sent < length) {
        ssize_t count = send(fd, text + sent, length - sent, 0);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            return -1;
        }
        sent += (size_t)count;
    }
    return 0;
}

static int send_request_line(int fd, const char *line) {
    char framed[256];
    int length = snprintf(framed, sizeof(framed), "%s\n", line);
    if (length <= 0 || (size_t)length >= sizeof(framed)) {
        return -1;
    }
    return send_all(fd, framed);
}

static int read_response_line(int fd, char *buffer, size_t capacity) {
    size_t used = 0;
    if (capacity < 2) {
        return -1;
    }
    while (used + 1 < capacity) {
        char byte;
        ssize_t count = recv(fd, &byte, 1, 0);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            return -1;
        }
        if (byte == '\n') {
            buffer[used] = '\0';
            return 0;
        }
        if (byte != '\r') {
            buffer[used++] = byte;
        }
    }
    buffer[used] = '\0';
    return -1;
}

static int expect_response(int fd, const char *request, const char *expected,
                           char *failure, size_t failure_capacity) {
    char observed[256];
    if (send_request_line(fd, request) != 0 ||
        read_response_line(fd, observed, sizeof(observed)) != 0) {
        fail_message(failure, failure_capacity, "socket request/response failed");
        return -1;
    }
    if (strcmp(observed, expected) != 0) {
        (void)snprintf(failure, failure_capacity,
                       "response mismatch: expected=%s observed=%s", expected, observed);
        return -1;
    }
    return 0;
}

static int expect_checksum_request(int fd, int request_id, int value,
                                   char *failure, size_t failure_capacity) {
    char request[128];
    char expected[128];
    (void)snprintf(request, sizeof(request),
                   "{\"op\":\"checksum\",\"request_id\":%d,\"value\":%d,\"hold_ms\":0}",
                   request_id, value);
    (void)snprintf(expected, sizeof(expected), "{\"id\":%d,\"checksum\":%d}",
                   request_id, value * value + 17);
    return expect_response(fd, request, expected, failure, failure_capacity);
}

static void *run_client(void *opaque) {
    Client *client = (Client *)opaque;
    int fd = connect_loopback(client->port);
    if (fd < 0) {
        client->result = -1;
        fail_message(client->failure, sizeof(client->failure), "connect failed");
        (void)pthread_barrier_wait(client->start_barrier);
        return NULL;
    }
    int barrier_status = pthread_barrier_wait(client->start_barrier);
    if (barrier_status != 0 && barrier_status != PTHREAD_BARRIER_SERIAL_THREAD) {
        client->result = -1;
        fail_message(client->failure, sizeof(client->failure), "client barrier failed");
        (void)close(fd);
        return NULL;
    }

    char request[160];
    if (client->role == 0) {
        (void)snprintf(request, sizeof(request),
                       "{\"op\":\"checksum\",\"request_id\":101,\"value\":20,\"hold_ms\":750}");
        if (expect_response(fd, request, "{\"id\":101,\"checksum\":417}",
                            client->failure, sizeof(client->failure)) != 0) {
            client->result = -1;
            (void)close(fd);
            return NULL;
        }
        for (int index = 0; index < client->soak_count; ++index) {
            int request_id = 10000 + index;
            int value = index % 100;
            if (expect_checksum_request(fd, request_id, value,
                                        client->failure, sizeof(client->failure)) != 0) {
                client->result = -1;
                (void)close(fd);
                return NULL;
            }
        }
    } else {
        if (expect_response(fd, "{broken", "{\"error\":\"BadJson\"}",
                            client->failure, sizeof(client->failure)) != 0) {
            client->result = -1;
            (void)close(fd);
            return NULL;
        }
        if (expect_checksum_request(fd, 202, 20,
                                    client->failure, sizeof(client->failure)) != 0) {
            client->result = -1;
            (void)close(fd);
            return NULL;
        }
        if (expect_response(fd,
                            "{\"op\":\"checksum\",\"request_id\":303,\"value\":1000,\"hold_ms\":0}",
                            "{\"id\":303,\"error\":\"ChecksumRange\"}",
                            client->failure, sizeof(client->failure)) != 0) {
            client->result = -1;
            (void)close(fd);
            return NULL;
        }
        for (int index = 0; index < client->soak_count; ++index) {
            int request_id = 20000 + index;
            int value = (index + 1) % 100;
            if (expect_checksum_request(fd, request_id, value,
                                        client->failure, sizeof(client->failure)) != 0) {
                client->result = -1;
                (void)close(fd);
                return NULL;
            }
        }
    }

    client->result = 0;
    (void)close(fd); /* EOF is the worker's close-and-release signal. */
    return NULL;
}

static int count_directory_entries(const char *path) {
    DIR *directory = opendir(path);
    if (directory == NULL) {
        return 0;
    }
    int count = 0;
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL) {
        if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0) {
            ++count;
        }
    }
    (void)closedir(directory);
    return count;
}

static void update_max(atomic_int *maximum, int observed) {
    int current = atomic_load(maximum);
    while (observed > current &&
           !atomic_compare_exchange_weak(maximum, &current, observed)) {
    }
}

static void *monitor_process(void *opaque) {
    ProcessMetrics *metrics = (ProcessMetrics *)opaque;
    char path[128];
    struct timespec pause = {.tv_sec = 0, .tv_nsec = 10000000};
    while (atomic_load(&metrics->stop) == 0) {
        (void)snprintf(path, sizeof(path), "/proc/%ld/fd", (long)metrics->pid);
        update_max(&metrics->max_fds, count_directory_entries(path));
        (void)snprintf(path, sizeof(path), "/proc/%ld/task", (long)metrics->pid);
        update_max(&metrics->max_threads, count_directory_entries(path));
        (void)nanosleep(&pause, NULL);
    }
    return NULL;
}

static pid_t start_service(const char *service_path, const char *source_path,
                           const char *trusted_path, int vm_mode, int *event_fd) {
    int events_pipe[2];
    if (pipe(events_pipe) != 0) {
        perror("pipe");
        return -1;
    }
    (void)fcntl(events_pipe[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(events_pipe[1], F_SETFD, FD_CLOEXEC);
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        (void)close(events_pipe[0]);
        (void)close(events_pipe[1]);
        return -1;
    }
    if (pid == 0) {
        int null_fd = open("/dev/null", O_WRONLY);
        (void)dup2(events_pipe[1], STDERR_FILENO);
        if (null_fd >= 0) {
            (void)dup2(null_fd, STDOUT_FILENO);
            (void)close(null_fd);
        }
        (void)close(events_pipe[0]);
        (void)close(events_pipe[1]);
        if (vm_mode) {
            const char *previous = getenv("LD_PRELOAD");
            char preload[4096];
            if (previous != NULL && previous[0] != '\0') {
                (void)snprintf(preload, sizeof(preload), "%s:%s", trusted_path, previous);
            } else {
                (void)snprintf(preload, sizeof(preload), "%s", trusted_path);
            }
            (void)setenv("LD_PRELOAD", preload, 1);
            char *const command[] = {(char *)service_path, "run", (char *)source_path, NULL};
            execvp(service_path, command);
        } else {
            char *const command[] = {(char *)service_path, NULL};
            execvp(service_path, command);
        }
        perror(vm_mode ? "exec Mimi VM" : "exec service");
        _exit(127);
    }
    (void)close(events_pipe[1]);
    *event_fd = events_pipe[0];
    return pid;
}

static int read_event_line(int fd, int timeout_ms, Event *event) {
    char line[128];
    size_t used = 0;
    uint64_t deadline = monotonic_ms() + (uint64_t)timeout_ms;
    while (used + 1 < sizeof(line)) {
        uint64_t now = monotonic_ms();
        if (now >= deadline) {
            return -1;
        }
        int remaining = (int)(deadline - now);
        struct pollfd descriptor = {.fd = fd, .events = POLLIN, .revents = 0};
        int polled = poll(&descriptor, 1, remaining);
        if (polled < 0 && errno == EINTR) {
            continue;
        }
        if (polled <= 0) {
            return -1;
        }
        char byte;
        ssize_t count = read(fd, &byte, 1);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            return -1;
        }
        if (byte == '\n') {
            line[used] = '\0';
            int parsed_code;
            int parsed_detail;
            if (sscanf(line, "W4_EVENT %d %d", &parsed_code, &parsed_detail) == 2) {
                event->code = parsed_code;
                event->detail = parsed_detail;
                (void)clock_gettime(CLOCK_MONOTONIC, &event->seen_at);
                return 0;
            }
            used = 0; /* ignore non-oracle diagnostics without losing later events */
        } else if (used + 1 < sizeof(line)) {
            line[used++] = byte;
        }
    }
    return -1;
}

static int find_event(int code, int detail) {
    for (size_t index = 0; index < event_count; ++index) {
        if (events[index].code == code &&
            (detail < 0 || events[index].detail == detail)) {
            return (int)index;
        }
    }
    return -1;
}

static int wait_event(int fd, int code, int detail, int timeout_ms) {
    int found = find_event(code, detail);
    if (found >= 0) {
        return found;
    }
    uint64_t deadline = monotonic_ms() + (uint64_t)timeout_ms;
    while (monotonic_ms() < deadline) {
        Event event;
        int remaining = (int)(deadline - monotonic_ms());
        if (read_event_line(fd, remaining, &event) != 0) {
            return -1;
        }
        if (event_count >= EVENT_LIMIT) {
            fprintf(stderr, "event oracle capacity exceeded\n");
            return -1;
        }
        events[event_count++] = event;
        if (event.code == code && (detail < 0 || event.detail == detail)) {
            return (int)(event_count - 1);
        }
    }
    return -1;
}

static int stop_service(int port) {
    int fd = connect_loopback(port);
    if (fd < 0) {
        return -1;
    }
    int result = expect_response(fd,
                                 "{\"op\":\"stop\",\"request_id\":900}",
                                 "{\"id\":900,\"stopping\":true}",
                                 (char[256]){0}, 256);
    (void)close(fd);
    return result;
}

static int check_closed_listener(int port) {
    int fd = connect_loopback(port);
    if (fd < 0) {
        return 0;
    }
    (void)close(fd);
    return -1;
}

static int run_cycle(const char *service_path, const char *source_path,
                     const char *trusted_path, int vm_mode,
                     int soak_count, int cycle,
                     long *peak_rss_kb, int *peak_fds, int *peak_threads) {
    int event_fd = -1;
    pid_t pid = start_service(service_path, source_path, trusted_path,
                              vm_mode, &event_fd);
    if (pid < 0) {
        return -1;
    }
    event_count = 0;
    int ready_index = wait_event(event_fd, W4_EVENT_READY, -1, 10000);
    if (ready_index < 0) {
        fprintf(stderr, "cycle %d: service did not publish READY\n", cycle);
        (void)kill(pid, SIGKILL);
        (void)waitpid(pid, NULL, 0);
        (void)close(event_fd);
        return -1;
    }
    int port = events[ready_index].detail;
    if (port < 1 || port > 65535) {
        fprintf(stderr, "cycle %d: invalid READY port %d\n", cycle, port);
        (void)kill(pid, SIGKILL);
        (void)waitpid(pid, NULL, 0);
        (void)close(event_fd);
        return -1;
    }

    ProcessMetrics metrics;
    metrics.pid = pid;
    atomic_init(&metrics.stop, 0);
    atomic_init(&metrics.max_fds, 0);
    atomic_init(&metrics.max_threads, 0);
    pthread_t monitor;
    if (pthread_create(&monitor, NULL, monitor_process, &metrics) != 0) {
        fprintf(stderr, "cycle %d: could not start process monitor\n", cycle);
        (void)kill(pid, SIGKILL);
        (void)waitpid(pid, NULL, 0);
        (void)close(event_fd);
        return -1;
    }

    pthread_barrier_t start_barrier;
    if (pthread_barrier_init(&start_barrier, NULL, 2) != 0) {
        fprintf(stderr, "cycle %d: could not initialize client barrier\n", cycle);
        atomic_store(&metrics.stop, 1);
        (void)pthread_join(monitor, NULL);
        (void)kill(pid, SIGKILL);
        (void)waitpid(pid, NULL, 0);
        (void)close(event_fd);
        return -1;
    }
    Client clients[2] = {
        {.port = port, .soak_count = soak_count, .role = 0, .result = -1,
         .failure = {0}, .start_barrier = &start_barrier},
        {.port = port, .soak_count = soak_count, .role = 1, .result = -1,
         .failure = {0}, .start_barrier = &start_barrier},
    };
    pthread_t client_threads[2];
    int thread_a = pthread_create(&client_threads[0], NULL, run_client, &clients[0]);
    int thread_b = pthread_create(&client_threads[1], NULL, run_client, &clients[1]);
    if (thread_a != 0 || thread_b != 0) {
        fprintf(stderr, "cycle %d: could not start both client threads\n", cycle);
        if (thread_a == 0 || thread_b == 0) {
            (void)pthread_barrier_wait(&start_barrier);
        }
        (void)kill(pid, SIGKILL);
        if (thread_a == 0) {
            (void)pthread_join(client_threads[0], NULL);
        }
        if (thread_b == 0) {
            (void)pthread_join(client_threads[1], NULL);
        }
        (void)pthread_barrier_destroy(&start_barrier);
        atomic_store(&metrics.stop, 1);
        (void)pthread_join(monitor, NULL);
        (void)waitpid(pid, NULL, 0);
        (void)close(event_fd);
        return -1;
    }

    int handoff_one = wait_event(event_fd, W4_EVENT_HANDOFF, 1, 10000);
    int handoff_two = wait_event(event_fd, W4_EVENT_HANDOFF, 2, 10000);
    int in_flight = wait_event(event_fd, W4_EVENT_IN_FLIGHT, 101, 10000);
    int bad_json = wait_event(event_fd, W4_EVENT_BAD_JSON, -1, 10000);
    int ffi_error = wait_event(event_fd, W4_EVENT_FFI_ERROR, 303, 10000);
    int cycle_ok = handoff_one >= 0 && handoff_two >= 0 && in_flight >= 0 &&
                   bad_json >= 0 && ffi_error >= 0;
    if (!cycle_ok) {
        fprintf(stderr, "cycle %d: missing worker handoff or in-flight event\n", cycle);
        (void)kill(pid, SIGKILL);
    } else if (stop_service(port) != 0) {
        fprintf(stderr, "cycle %d: cooperative stop response mismatch\n", cycle);
        cycle_ok = 0;
        (void)kill(pid, SIGKILL);
    }

    int stop_event = -1;
    int response_event = -1;
    int worker_one = -1;
    int worker_two = -1;
    int drained_event = -1;
    if (cycle_ok) {
        stop_event = wait_event(event_fd, W4_EVENT_STOP_ACCEPTED, 900, 10000);
        response_event = wait_event(event_fd, W4_EVENT_RESPONSE, 101, 10000);
        worker_one = wait_event(event_fd, W4_EVENT_WORKER_DONE, 1, 10000);
        worker_two = wait_event(event_fd, W4_EVENT_WORKER_DONE, 2, 10000);
        drained_event = wait_event(event_fd, W4_EVENT_DRAINED, 0, 10000);
        if (stop_event < 0 || response_event < 0 || worker_one < 0 ||
            worker_two < 0 || drained_event < 0 ||
            !(in_flight < stop_event && stop_event < response_event &&
              worker_one < drained_event && worker_two < drained_event)) {
            fprintf(stderr, "cycle %d: stop/drain event order mismatch\n", cycle);
            cycle_ok = 0;
        }
    }

    (void)pthread_join(client_threads[0], NULL);
    (void)pthread_join(client_threads[1], NULL);
    (void)pthread_barrier_destroy(&start_barrier);
    if (clients[0].result != 0 || clients[1].result != 0) {
        fprintf(stderr, "cycle %d: client failure: A=%s B=%s\n", cycle,
                clients[0].failure, clients[1].failure);
        cycle_ok = 0;
    }
    if (cycle_ok && check_closed_listener(port) != 0) {
        fprintf(stderr, "cycle %d: listener still accepted after DRAINED\n", cycle);
        cycle_ok = 0;
    }

    int status = 0;
    struct rusage usage;
    memset(&usage, 0, sizeof(usage));
    pid_t waited = waitpid(pid, &status, 0);
    (void)getrusage(RUSAGE_CHILDREN, &usage);
    atomic_store(&metrics.stop, 1);
    (void)pthread_join(monitor, NULL);
    (void)close(event_fd);
    if (waited != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "cycle %d: service exited abnormally (status=%d)\n", cycle,
                status);
        cycle_ok = 0;
    }
    if (usage.ru_maxrss > *peak_rss_kb) {
        *peak_rss_kb = usage.ru_maxrss;
    }
    if (atomic_load(&metrics.max_fds) > *peak_fds) {
        *peak_fds = atomic_load(&metrics.max_fds);
    }
    if (atomic_load(&metrics.max_threads) > *peak_threads) {
        *peak_threads = atomic_load(&metrics.max_threads);
    }
    if (cycle_ok) {
        printf("W4_CYCLE_OK cycle=%d port=%d requests_per_client=%d events=%zu\n",
               cycle, port, soak_count, event_count);
    }
    return cycle_ok ? 0 : -1;
}

int main(int argc, char **argv) {
    int vm_mode = argc > 1 && strcmp(argv[1], "--vm") == 0;
    int native_mode = argc > 1 && strcmp(argv[1], "--native") == 0;
    if ((!vm_mode && !native_mode) ||
        (native_mode && (argc < 5 || argc > 7)) ||
        (vm_mode && (argc < 6 || argc > 8))) {
        fprintf(stderr,
                "usage: %s --native SERVICE EXPORTS_SO TRUSTED_SO [SOAK_REQUESTS] [ROUNDS]\n"
                "   or: %s --vm MIMI SOURCE EXPORTS_SO TRUSTED_SO [SOAK_REQUESTS] [ROUNDS]\n",
                argv[0], argv[0]);
        return 2;
    }
    const char *service_path = argv[2];
    const char *source_path = vm_mode ? argv[3] : NULL;
    int exports_index = vm_mode ? 4 : 3;
    int trusted_index = vm_mode ? 5 : 4;
    int soak_index = trusted_index + 1;
    int rounds_index = trusted_index + 2;
    int soak_count = argc > soak_index ? atoi(argv[soak_index]) : 64;
    int rounds = argc > rounds_index ? atoi(argv[rounds_index]) : 1;
    if (soak_count < 0 || soak_count > 250 || rounds < 1 || rounds > 100) {
        fprintf(stderr, "SOAK_REQUESTS must be 0..250 and ROUNDS must be 1..100\n");
        return 2;
    }
    const char *exports_path = argv[exports_index];
    const char *trusted_path = argv[trusted_index];
    if (check_c_oracle(trusted_path, exports_path) != 0) {
        return 1;
    }

    const char *slash = strrchr(trusted_path, '/');
    char library_dir[2048];
    if (slash == NULL) {
        fail_message(library_dir, sizeof(library_dir), ".");
    } else {
        size_t directory_length = (size_t)(slash - trusted_path);
        if (directory_length >= sizeof(library_dir)) {
            fprintf(stderr, "trusted library directory path too long\n");
            return 2;
        }
        memcpy(library_dir, trusted_path, directory_length);
        library_dir[directory_length] = '\0';
    }
    const char *old_library_path = getenv("LD_LIBRARY_PATH");
    char library_path[4096];
    if (old_library_path != NULL && old_library_path[0] != '\0') {
        (void)snprintf(library_path, sizeof(library_path), "%s:%s",
                       library_dir, old_library_path);
    } else {
        (void)snprintf(library_path, sizeof(library_path), "%s", library_dir);
    }
    if (setenv("LD_LIBRARY_PATH", library_path, 1) != 0) {
        perror("setenv LD_LIBRARY_PATH");
        return 2;
    }

    long peak_rss_kb = 0;
    int peak_fds = 0;
    int peak_threads = 0;
    for (int cycle = 1; cycle <= rounds; ++cycle) {
        if (run_cycle(service_path, source_path, trusted_path, vm_mode,
                      soak_count, cycle, &peak_rss_kb,
                      &peak_fds, &peak_threads) != 0) {
            return 1;
        }
    }
    printf("W4_METRICS mode=%s rounds=%d requests_per_client=%d peak_rss_kb=%ld "
           "peak_child_fds=%d peak_child_threads=%d\n",
           vm_mode ? "vm" : "native", rounds, soak_count, peak_rss_kb,
           peak_fds, peak_threads);
    return 0;
}
