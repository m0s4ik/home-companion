// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// The protocol between the body (home-companion) and the web view helper (home-companion-web): SOCK_SEQPACKET Unix
// socket, one message per packet, 8-byte header, little-endian fields, dma-buf fds as
// SCM_RIGHTS. Reference: web/PROTOCOL.md. Kept in sync by hand with crates/webview-proto
// (its tests check the same sizes and byte layout).
//
// The structs below are the wire layout on a little-endian host (aarch64, x86-64): every
// field is naturally aligned, so there is no padding the compiler could add.

#ifndef HC_WEB_PROTOCOL_H
#define HC_WEB_PROTOCOL_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "protocol.h: the wire format is little-endian; add byte swapping for this host"
static inline int hcw_send_confirm(int sock, struct hcw_builder *b, uint32_t id, uint32_t kind,
                                    const char *path)
{
    struct hcw_confirm *c = (struct hcw_confirm *)hcw_build_begin(b, HCW_CONFIRM, sizeof *c);
    c->id = id;
    c->kind = kind;
    c->path_len = (uint32_t)strlen(path);
    if (hcw_build_bytes(b, path, c->path_len))
        return -1;
    return hcw_build_send(sock, b);
}

// Walks a received HCW_GUARD: calls fn(path, len, data) for each path (not NUL-terminated).
// Returns the number of paths, or -1 if the lengths do not fit the message.
static inline int hcw_guard_paths(const struct hcw_guard *m,
                                   void (*fn)(const char *path, uint32_t len, void *data), void *data)
{
    if (m->h.length < sizeof *m)
        return -1;
    const uint8_t *p = (const uint8_t *)(m + 1), *end = (const uint8_t *)m + m->h.length;
    for (uint32_t i = 0; i < m->n_paths; i++) {
        uint32_t len;
        if (end - p < 4)
            return -1;
        memcpy(&len, p, 4);
        p += 4;
        if ((uint64_t)HCW_PAD4((uint64_t)len) > (uint64_t)(end - p))
            return -1;
        fn((const char *)p, len, data);
        p += HCW_PAD4(len);
    }
    return (int)m->n_paths;
}

#endif

#define HCW_VERSION 2u // 2: pause, resume
#define HCW_HEADER_SIZE 8u
#define HCW_MAX_MESSAGE 65536u
#define HCW_MAX_PLANES 4u

#define HCW_MOD_INVALID 0x00ffffffffffffffull
#define HCW_MOD_LINEAR 0ull

// Message types. Direction: H = helper -> body, B = body -> helper.
enum hcw_type {
    HCW_HELLO = 1,        // both
    HCW_FRAME = 2,        // H, fds the first time a buffer id is sent
    HCW_DROP = 3,         // H
    HCW_RELEASE = 4,      // B
    HCW_PRESENTED = 5,    // B
    HCW_SHOW = 6,         // B
    HCW_HIDE = 7,         // B
    HCW_TOUCH = 8,        // B
    HCW_MENU = 9,         // H, variable
    HCW_MENU_RESULT = 10, // B
    HCW_LOAD = 11,        // B, variable
    HCW_RELOAD = 12,      // B
    HCW_BACK = 13,        // B
    HCW_STATE = 14,       // H, variable
    HCW_CONFIRM = 15,     // H, variable
    HCW_CONFIRM_RESULT = 16, // B
    HCW_GUARD = 17,       // B, variable
    HCW_PAUSE = 18,       // B: stop the web process (the view is hidden for long); version 2
    HCW_RESUME = 19,      // B: let it run again (also on show and when the body goes)
};

struct hcw_header {
    uint16_t type;
    uint16_t flags;  // 0
    uint32_t length; // whole message, header included
};

struct hcw_hello {
    struct hcw_header h;
    uint32_t version;
    uint32_t width;  // view size in pixels
    uint32_t height;
    uint32_t scale_milli; // device scale x 1000
};

struct hcw_plane {
    uint32_t offset;
    uint32_t stride;
};

#define HCW_FRAME_FDS 1u // frame.flags: one fd per plane follows as SCM_RIGHTS

struct hcw_frame {
    struct hcw_header h;
    uint32_t buffer; // helper-assigned id, stable until HCW_DROP
    uint32_t width;
    uint32_t height;
    uint32_t fourcc;   // DRM_FORMAT_*
    uint64_t modifier; // DRM_FORMAT_MOD_* (HCW_MOD_INVALID = implicit layout)
    uint32_t n_planes; // 1..4
    uint32_t flags;    // HCW_FRAME_FDS
    uint64_t seq;      // helper's frame counter
    uint64_t time_ns;  // CLOCK_MONOTONIC when the frame was ready
    struct hcw_plane planes[HCW_MAX_PLANES]; // unused planes zero
};

struct hcw_buffer_msg { // HCW_DROP and HCW_RELEASE
    struct hcw_header h;
    uint32_t buffer;
    uint32_t reserved;
};

#define HCW_PRESENTED_SKIPPED 1u // presented.flags: the frame never reached the screen

// The answer to every frame, exactly once (pace rendering on it).
struct hcw_presented {
    struct hcw_header h;
    uint32_t buffer;
    uint32_t flags;   // HCW_PRESENTED_SKIPPED
    uint64_t seq;     // the frame's seq
    uint64_t time_ns; // CLOCK_MONOTONIC of the page flip
};

enum hcw_touch_kind {
    HCW_TOUCH_DOWN = 0,
    HCW_TOUCH_MOTION = 1,
    HCW_TOUCH_UP = 2,
    HCW_TOUCH_CANCEL = 3,
};

struct hcw_touch {
    struct hcw_header h;
    uint32_t kind; // enum hcw_touch_kind
    int32_t id;    // touch point id (stable from down to up/cancel)
    float x;       // view pixels, origin top left
    float y;
    uint64_t time_ns; // CLOCK_MONOTONIC
};

#define HCW_MENU_ITEM_ENABLED 1u
#define HCW_MENU_ITEM_GROUP 2u // <optgroup> label, not selectable

// Followed by n_items x { uint32_t flags; uint32_t label_len; label bytes (UTF-8, no NUL),
// zero-padded to a multiple of 4 }.
struct hcw_menu {
    struct hcw_header h;
    uint32_t menu;     // id, echoed in HCW_MENU_RESULT
    int32_t selected;  // -1 = none
    int32_t x, y, width, height; // the <select> element, view pixels
    uint32_t n_items;
};

struct hcw_menu_result {
    struct hcw_header h;
    uint32_t menu;
    int32_t index; // -1 = cancelled
};

// Followed by url_len bytes (UTF-8), zero-padded to a multiple of 4.
struct hcw_load {
    struct hcw_header h;
    uint32_t url_len;
};

enum hcw_load_state {
    HCW_STATE_LOADING = 0,
    HCW_STATE_READY = 1,
    HCW_STATE_ERROR = 2,
};

// Followed by the title, then the message (UTF-8), each zero-padded to a multiple of 4.
struct hcw_state {
    struct hcw_header h;
    uint32_t state;          // enum hcw_load_state
    uint32_t progress_milli; // 0..1000
    uint32_t title_len;
    uint32_t message_len;
};

#define HCW_CONFIRM_HEAVY_VIEW 0u

// The page wants to open a guarded view. Followed by path_len bytes (UTF-8), zero-padded to 4.
struct hcw_confirm {
    struct hcw_header h;
    uint32_t id;   // echoed in HCW_CONFIRM_RESULT
    uint32_t kind; // HCW_CONFIRM_HEAVY_VIEW
    uint32_t path_len;
};

struct hcw_confirm_result {
    struct hcw_header h;
    uint32_t id;
    int32_t accepted; // 1 = open it, 0 = stay
};

// Path prefixes of guarded (heavy) views, replacing the previous list. Followed by n_paths x
// { uint32_t len; bytes (UTF-8), zero-padded to a multiple of 4 }.
struct hcw_guard {
    struct hcw_header h;
    uint32_t n_paths;
};

_Static_assert(sizeof(struct hcw_header) == 8, "header");
_Static_assert(sizeof(struct hcw_hello) == 8 + 16, "hello");
_Static_assert(sizeof(struct hcw_frame) == 8 + 80, "frame");
_Static_assert(offsetof(struct hcw_frame, modifier) == 24, "frame.modifier");
_Static_assert(offsetof(struct hcw_frame, planes) == 56, "frame.planes");
_Static_assert(sizeof(struct hcw_buffer_msg) == 8 + 8, "drop/release");
_Static_assert(sizeof(struct hcw_presented) == 8 + 24, "presented");
_Static_assert(sizeof(struct hcw_touch) == 8 + 24, "touch");
_Static_assert(sizeof(struct hcw_menu) == 8 + 28, "menu");
_Static_assert(sizeof(struct hcw_menu_result) == 8 + 8, "menu_result");
_Static_assert(sizeof(struct hcw_load) == 8 + 4, "load");
_Static_assert(sizeof(struct hcw_state) == 8 + 16, "state");
_Static_assert(sizeof(struct hcw_confirm) == 8 + 12, "confirm");
_Static_assert(sizeof(struct hcw_confirm_result) == 8 + 8, "confirm_result");
_Static_assert(sizeof(struct hcw_guard) == 8 + 4, "guard");

#define HCW_PAD4(n) (((n) + 3u) & ~3u)

static inline void hcw_header_init(struct hcw_header *h, uint16_t type, uint32_t length)
{
    h->type = type;
    h->flags = 0;
    h->length = length;
}

// Sends one message (msg->length bytes) with nfds fds (0..4). Returns 0 or -1 with errno.
// Never raises SIGPIPE. The caller keeps (and later closes) its fds.
static inline int hcw_send(int sock, const void *msg, const int *fds, unsigned nfds)
{
    const struct hcw_header *h = (const struct hcw_header *)msg;
    union {
        char buf[CMSG_SPACE(sizeof(int) * HCW_MAX_PLANES)];
        struct cmsghdr align;
    } control;
    struct iovec iov = { .iov_base = (void *)msg, .iov_len = h->length };
    struct msghdr mh = { .msg_iov = &iov, .msg_iovlen = 1 };
    if (nfds > HCW_MAX_PLANES || h->length < HCW_HEADER_SIZE || h->length > HCW_MAX_MESSAGE) {
        errno = EINVAL;
        return -1;
    }
    if (nfds) {
        memset(&control, 0, sizeof control);
        mh.msg_control = control.buf;
        mh.msg_controllen = CMSG_SPACE(sizeof(int) * nfds);
        struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int) * nfds);
        memcpy(CMSG_DATA(c), fds, sizeof(int) * nfds);
    }
    ssize_t r;
    do
        r = sendmsg(sock, &mh, MSG_NOSIGNAL);
    while (r < 0 && errno == EINTR);
    return r < 0 ? -1 : 0;
}

// Receives one message into buf (cap >= HCW_MAX_MESSAGE recommended) and up to 4 fds
// (owned by the caller afterwards, close-on-exec). Returns the message length, 0 on EOF, -1
// with errno (EAGAIN on a non-blocking socket, EMSGSIZE if truncated, EPROTO if the header's
// length does not match the packet; fds of a bad message are closed). Unknown types are
// returned as they are: the caller skips them (and closes any fds).
static inline ssize_t hcw_recv(int sock, void *buf, size_t cap, int *fds, unsigned *nfds)
{
    union {
        char buf[CMSG_SPACE(sizeof(int) * HCW_MAX_PLANES)];
        struct cmsghdr align;
    } control;
    struct iovec iov = { .iov_base = buf, .iov_len = cap };
    struct msghdr mh = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control.buf,
                         .msg_controllen = sizeof control.buf };
    ssize_t n;
    *nfds = 0;
    do
        n = recvmsg(sock, &mh, MSG_CMSG_CLOEXEC);
    while (n < 0 && errno == EINTR);
    if (n < 0)
        return -1;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c)) {
        if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS)
            continue;
        unsigned k = (unsigned)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
        for (unsigned i = 0; i < k; i++) {
            int fd;
            memcpy(&fd, CMSG_DATA(c) + i * sizeof(int), sizeof fd);
            if (*nfds < HCW_MAX_PLANES)
                fds[(*nfds)++] = fd;
            else
                close(fd);
        }
    }
    int bad = 0;
    if (mh.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) {
        bad = EMSGSIZE;
    } else if (n > 0 && (n < (ssize_t)HCW_HEADER_SIZE ||
                         ((const struct hcw_header *)buf)->length != (uint32_t)n)) {
        bad = EPROTO;
    }
    if (bad) {
        for (unsigned i = 0; i < *nfds; i++)
            close(fds[i]);
        *nfds = 0;
        errno = bad;
        return -1;
    }
    return n;
}

// Building the variable messages (menu, load, state): start, append, send.
struct hcw_builder {
    uint8_t data[HCW_MAX_MESSAGE];
    uint32_t len;
};

// Starts a message whose fixed part (header included) is `fixed` bytes, zeroed. Returns a
// pointer to it for the caller to fill.
static inline void *hcw_build_begin(struct hcw_builder *b, uint16_t type, uint32_t fixed)
{
    memset(b->data, 0, fixed);
    b->len = fixed;
    hcw_header_init((struct hcw_header *)b->data, type, fixed);
    return b->data;
}

// Appends bytes padded to 4. Returns 0, or -1 if the message would exceed HCW_MAX_MESSAGE.
static inline int hcw_build_bytes(struct hcw_builder *b, const void *p, uint32_t n)
{
    if (HCW_PAD4(n) > HCW_MAX_MESSAGE - b->len)
        return -1;
    memcpy(b->data + b->len, p, n);
    memset(b->data + b->len + n, 0, HCW_PAD4(n) - n);
    b->len += HCW_PAD4(n);
    return 0;
}

static inline int hcw_build_u32(struct hcw_builder *b, uint32_t v)
{
    return hcw_build_bytes(b, &v, 4);
}

// Appends one menu item.
static inline int hcw_build_menu_item(struct hcw_builder *b, uint32_t flags, const char *label)
{
    uint32_t n = (uint32_t)strlen(label);
    if (hcw_build_u32(b, flags) || hcw_build_u32(b, n) || hcw_build_bytes(b, label, n))
        return -1;
    ((struct hcw_menu *)b->data)->n_items++;
    return 0;
}

static inline int hcw_build_send(int sock, struct hcw_builder *b)
{
    ((struct hcw_header *)b->data)->length = b->len;
    return hcw_send(sock, b->data, NULL, 0);
}

static inline int hcw_send_state(int sock, struct hcw_builder *b, enum hcw_load_state state,
                                  uint32_t progress_milli, const char *title, const char *message)
{
    struct hcw_state *s = (struct hcw_state *)hcw_build_begin(b, HCW_STATE, sizeof *s);
    s->state = state;
    s->progress_milli = progress_milli;
    s->title_len = (uint32_t)strlen(title);
    s->message_len = (uint32_t)strlen(message);
    if (hcw_build_bytes(b, title, s->title_len) || hcw_build_bytes(b, message, s->message_len))
        return -1;
    return hcw_build_send(sock, b);
}

// The string after a received HCW_LOAD's fixed part, or NULL if the lengths do not fit.
// Not NUL-terminated: use url_len.
static inline const char *hcw_load_url(const struct hcw_load *m)
{
    if (m->h.length < sizeof *m || m->url_len > m->h.length - sizeof *m)
        return NULL;
    return (const char *)(m + 1);
}

static inline int hcw_send_confirm(int sock, struct hcw_builder *b, uint32_t id, uint32_t kind,
                                    const char *path)
{
    struct hcw_confirm *c = (struct hcw_confirm *)hcw_build_begin(b, HCW_CONFIRM, sizeof *c);
    c->id = id;
    c->kind = kind;
    c->path_len = (uint32_t)strlen(path);
    if (hcw_build_bytes(b, path, c->path_len))
        return -1;
    return hcw_build_send(sock, b);
}

// Walks a received HCW_GUARD: calls fn(path, len, data) for each path (not NUL-terminated).
// Returns the number of paths, or -1 if the lengths do not fit the message.
static inline int hcw_guard_paths(const struct hcw_guard *m,
                                   void (*fn)(const char *path, uint32_t len, void *data), void *data)
{
    if (m->h.length < sizeof *m)
        return -1;
    const uint8_t *p = (const uint8_t *)(m + 1), *end = (const uint8_t *)m + m->h.length;
    for (uint32_t i = 0; i < m->n_paths; i++) {
        uint32_t len;
        if (end - p < 4)
            return -1;
        memcpy(&len, p, 4);
        p += 4;
        if ((uint64_t)HCW_PAD4((uint64_t)len) > (uint64_t)(end - p))
            return -1;
        fn((const char *)p, len, data);
        p += HCW_PAD4(len);
    }
    return (int)m->n_paths;
}

#endif
