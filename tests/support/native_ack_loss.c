/* Explicit macOS fault configuration for the ordinary-binary acceptance row.
 * This library is never linked into Dolgorae or installed with its skill.
 * Only the selected Worker's Accepted send and the selected serve Run's next
 * lifecycle acquisition can wait. Witnesses are private test observations.
 */
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

static _Thread_local int internal;
static atomic_int request_selected, ack_selected, transport_failed;
static atomic_int selected_fd = -1;
static dev_t selected_device;
static ino_t selected_inode;

static int path_for(const char *name, char path[PATH_MAX]) {
    const char *root = getenv("DOLGORAE_NATIVE_FAULT_DIR");
    return root && root[0] == '/' &&
        snprintf(path, PATH_MAX, "%s/%s", root, name) < PATH_MAX;
}

static int config(const char *name, char *value, size_t size) {
    char path[PATH_MAX];
    if (!path_for(name, path)) return 0;
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return 0;
    ssize_t count = read(fd, value, size - 1);
    close(fd);
    if (count <= 0) return 0;
    value[count] = '\0';
    value[strcspn(value, "\r\n")] = '\0';
    return 1;
}

static void witness(const char *name, const void *bytes, size_t size) {
    char path[PATH_MAX];
    if (!path_for(name, path)) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return;
    const char *cursor = bytes;
    while (size) {
        ssize_t count = write(fd, cursor, size);
        if (count <= 0) break;
        cursor += count;
        size -= (size_t)count;
    }
    fsync(fd);
    close(fd);
}

static int selected(const char *role) {
    char name[64], value[128];
    unsigned pid, uid;
    unsigned long long seconds, micros;
    snprintf(name, sizeof(name), "%s.identity", role);
    if (!config(name, value, sizeof(value)) ||
        sscanf(value, "%u %u %llu %llu", &pid, &uid, &seconds, &micros) != 4 ||
        pid != (unsigned)getpid() || uid != (unsigned)geteuid()) return 0;
    struct proc_bsdinfo process;
    return proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &process, sizeof(process)) ==
            sizeof(process) && process.pbi_uid == uid &&
        process.pbi_start_tvsec == seconds && process.pbi_start_tvusec == micros;
}

static int control_socket(int fd) {
    char expected[PATH_MAX], identity[128];
    unsigned long long device, inode;
    struct stat observed;
    if (!config("control.socket", expected, sizeof(expected))) return 0;
    /* A dead Worker's socket pathname can be retired before the caller reads
     * its error. Bind the selected sender's still-open connection, rather than
     * requiring that stale pathname to remain the current runtime locator. */
    if (fd == atomic_load_explicit(&selected_fd, memory_order_acquire) && !fstat(fd, &observed) &&
        S_ISSOCK(observed.st_mode) && observed.st_dev == selected_device &&
        observed.st_ino == selected_inode) {
        struct sockaddr_un peer = {0};
        socklen_t length = sizeof(peer);
        if (!getpeername(fd, (struct sockaddr *)&peer, &length) &&
            peer.sun_family == AF_UNIX && !strcmp(peer.sun_path, expected)) return 1;
    }
    if (
        !config("control.identity", identity, sizeof(identity)) ||
        sscanf(identity, "%llu %llu", &device, &inode) != 2 ||
        lstat(expected, &observed) || !S_ISSOCK(observed.st_mode) ||
        observed.st_uid != geteuid() || (unsigned long long)observed.st_dev != device ||
        (unsigned long long)observed.st_ino != inode) return 0;
    struct sockaddr_un address = {0};
    socklen_t size = sizeof(address);
    if (!getsockname(fd, (struct sockaddr *)&address, &size) &&
        address.sun_family == AF_UNIX && !strcmp(address.sun_path, expected)) return 1;
    memset(&address, 0, sizeof(address));
    size = sizeof(address);
    return !getpeername(fd, (struct sockaddr *)&address, &size) &&
        address.sun_family == AF_UNIX && !strcmp(address.sun_path, expected);
}

static void hold(const char *release) {
    char path[PATH_MAX], value[128];
    unsigned pid;
    unsigned long long until;
    if (!path_for(release, path) || !config("harness.pid", value, sizeof(value)) ||
        sscanf(value, "%u", &pid) != 1 || pid < 2 ||
        !config("until.monotonic-ns", value, sizeof(value)) ||
        sscanf(value, "%llu", &until) != 1) {
        witness("invalid-hold-config", "invalid\n", 8);
        return;
    }
    struct timespec now, pause = {.tv_nsec = 10000000};
    while (access(path, F_OK)) {
        clock_gettime(CLOCK_MONOTONIC, &now);
        unsigned long long current = (unsigned long long)now.tv_sec * 1000000000 + now.tv_nsec;
        if ((kill((pid_t)pid, 0) && errno == ESRCH) || current >= until) {
            witness("fail-safe-release", release, strlen(release));
            break;
        }
        nanosleep(&pause, NULL);
    }
}

/* write_control_request sends its first byte with SCM_RIGHTS, then passes the
 * complete remaining serialized request to write_all. Observe that sender-side
 * buffer rather than assembling recvmsg/read fragments or copying credential
 * descriptors. The existing Rust type and identity oracle validates the copied
 * request before release-selection lets the unchanged bytes reach the Worker.
 */
static void select_request(int fd, const void *bytes, size_t size, const char *call) {
    const char prefix[] = "\"operation\":\"external_submit\",";
    char marker[128], engagement[128];
    if (!selected("serve") || size < sizeof(prefix) ||
        memcmp(bytes, prefix, sizeof(prefix) - 1) ||
        !config("request.match", marker, sizeof(marker)) ||
        !config("engagement.match", engagement, sizeof(engagement))) return;
    if (size > 8 * 1024 * 1024 || ((const char *)bytes)[size - 1] != '}') {
        witness("request-selection-unsupported", call, strlen(call));
        return;
    }
    char *frame = malloc(size + 2);
    if (!frame) return;
    frame[0] = '{';
    memcpy(frame + 1, bytes, size);
    frame[size + 1] = '\0';
    if (strstr(frame, marker) && strstr(frame, engagement) &&
        !atomic_exchange(&request_selected, 1)) {
        struct sockaddr_un address = {0};
        socklen_t length = sizeof(address);
        struct stat socket;
        if (!getpeername(fd, (struct sockaddr *)&address, &length) &&
            address.sun_family == AF_UNIX && address.sun_path[0] &&
            !lstat(address.sun_path, &socket) && S_ISSOCK(socket.st_mode) &&
            socket.st_uid == geteuid()) {
            struct stat connection;
            if (fstat(fd, &connection) || !S_ISSOCK(connection.st_mode)) {
                free(frame);
                return;
            }
            selected_device = connection.st_dev;
            selected_inode = connection.st_ino;
            atomic_store_explicit(&selected_fd, fd, memory_order_release);
            char identity[128];
            snprintf(identity, sizeof(identity), "%llu %llu\n",
                (unsigned long long)socket.st_dev, (unsigned long long)socket.st_ino);
            witness("selected-request.socket", address.sun_path, strlen(address.sun_path));
            witness("selected-request.socket-identity", identity, strlen(identity));
            witness("selected-request.json", frame, size + 1);
            witness("selected-request.syscall", call, strlen(call));
            witness("selected-request.ready", "ready\n", 6);
            hold("release-selection");
        }
    }
    free(frame);
}

static void accepted_send(int fd, const void *bytes, size_t size, const char *call) {
    const char prefix[] = "{\"result\":\"accepted\",\"accepted\":";
    if (!selected("worker") || !control_socket(fd) || size < sizeof(prefix) ||
        size > 32768 || memcmp(bytes, prefix, sizeof(prefix) - 1) ||
        ((const char *)bytes)[size - 1] != '}') return;
    if (atomic_exchange(&ack_selected, 1)) {
        witness("duplicate-accepted-send", call, strlen(call));
        return;
    }
    witness("accepted.frame", bytes, size);
    witness("accepted.syscall", call, strlen(call));
    witness("accepted.ready", "ready\n", 6);
    hold("release-ack");
}

static ssize_t fault_write(int fd, const void *bytes, size_t size) {
    if (!internal) {
        internal = 1;
        select_request(fd, bytes, size, "write");
        accepted_send(fd, bytes, size, "write");
        internal = 0;
    }
    return write(fd, bytes, size);
}

static ssize_t fault_send(int fd, const void *bytes, size_t size, int flags) {
    if (!internal) {
        internal = 1;
        select_request(fd, bytes, size, "send");
        accepted_send(fd, bytes, size, "send");
        internal = 0;
    }
    return send(fd, bytes, size, flags);
}

static void transport_result(int fd, ssize_t result, int error, const char *call) {
    if ((result == 0 || (result < 0 && error != EAGAIN && error != EINTR)) &&
        selected("serve") && control_socket(fd)) {
        char observed[128];
        snprintf(observed, sizeof(observed), "%s result=%lld errno=%d\n", call, (long long)result, error);
        witness("control.transport", observed, strlen(observed));
        if (result == 0 || (result < 0 && error == ECONNRESET)) {
            atomic_store(&transport_failed, 1);
            witness("control.failure", observed, strlen(observed));
        }
    }
}

static ssize_t fault_read(int fd, void *bytes, size_t size) {
    ssize_t result = read(fd, bytes, size);
    int observed_errno = errno;
    if (!internal && size) {
        internal = 1;
        transport_result(fd, result, result < 0 ? observed_errno : 0, "read");
        internal = 0;
    }
    errno = observed_errno;
    return result;
}

static ssize_t fault_recv(int fd, void *bytes, size_t size, int flags) {
    ssize_t result = recv(fd, bytes, size, flags);
    int observed_errno = errno;
    if (!internal && size) {
        internal = 1;
        transport_result(fd, result, result < 0 ? observed_errno : 0, "recv");
        internal = 0;
    }
    errno = observed_errno;
    return result;
}

static int fault_poll(struct pollfd *descriptors, nfds_t count, int timeout) {
    int result = poll(descriptors, count, timeout);
    int observed_errno = errno;
    if (!internal && result > 0 && count == 1 &&
        descriptors[0].fd == atomic_load_explicit(&selected_fd, memory_order_acquire)) {
        internal = 1;
        struct stat connection;
        if (selected("serve") && !fstat(descriptors[0].fd, &connection) &&
            S_ISSOCK(connection.st_mode) && connection.st_dev == selected_device &&
            connection.st_ino == selected_inode) {
            char observed[128];
            snprintf(observed, sizeof(observed), "poll result=%d revents=%d\n", result, descriptors[0].revents);
            witness("control.poll", observed, strlen(observed));
            if (descriptors[0].revents & POLLHUP) {
                atomic_store(&transport_failed, 1);
                witness("control.failure", observed, strlen(observed));
            }
        }
        internal = 0;
    }
    errno = observed_errno;
    return result;
}

static int fault_flock(int fd, int operation) {
    if (!internal && operation == (LOCK_EX | LOCK_NB) && atomic_load(&transport_failed)) {
        internal = 1;
        char expected[PATH_MAX], actual[PATH_MAX];
        if (selected("serve") && config("lifecycle.lock", expected, sizeof(expected)) &&
            !fcntl(fd, F_GETPATH, actual) && !strcmp(expected, actual)) {
            witness("recovery.held", actual, strlen(actual));
            witness("recovery.ready", "ready\n", 6);
            hold("release-recovery");
        }
        internal = 0;
    }
    return flock(fd, operation);
}

__attribute__((constructor)) static void loaded(void) {
    internal = 1;
    char name[64];
    snprintf(name, sizeof(name), "loaded-%d", getpid());
    witness(name, "native-ack-loss/v1\n", 19);
    internal = 0;
}

/* The public SDK omits dyld-interposing.h. These are its fixed two-pointer
 * Mach-O interpose records, for only the concrete ABIs used above. */
__attribute__((used, section("__DATA,__interpose")))
static const struct { const void *replacement; const void *original; } interposes[] = {
    {(const void *)fault_write, (const void *)write},
    {(const void *)fault_send, (const void *)send},
    {(const void *)fault_read, (const void *)read},
    {(const void *)fault_recv, (const void *)recv},
    {(const void *)fault_poll, (const void *)poll},
    {(const void *)fault_flock, (const void *)flock},
};
