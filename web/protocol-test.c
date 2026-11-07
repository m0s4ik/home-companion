// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// Checks web/home-companion-web/protocol.h against the bytes the Rust crate produces (the same hex
// strings as `golden` in crates/webview-proto/src/lib.rs) and passes fds over a socketpair.
//   cc -std=c11 -Wall -Wextra -Werror -o /tmp/protocol-test web/protocol-test.c && /tmp/protocol-test

#define _GNU_SOURCE
#include "home-companion-web/protocol.h"

#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>

static int failures;

static void expect_hex(const char *what, const void *msg, const char *hex)
{
    const uint8_t *b = msg;
    uint32_t n = ((const struct hcw_header *)msg)->length;
    char got[2 * HCW_MAX_MESSAGE + 1];
    for (uint32_t i = 0; i < n; i++)
        sprintf(got + 2 * i, "%02x", b[i]);
    got[2 * n] = 0;
    if (strcmp(got, hex)) {
        printf("FAIL %s\n  got  %s\n  want %s\n", what, got, hex);
        failures++;
    } else {
        printf("ok   %s (%u bytes)\n", what, n);
    }
}

static void count_path(const char *path, uint32_t len, void *data)
{
    (void)path;
    *(unsigned *)data += len;
}

int main(void)
{
    struct hcw_frame f = { 0 };
    hcw_header_init(&f.h, HCW_FRAME, sizeof f);
    f.buffer = 7;
    f.width = 1280;
    f.height = 800;
    f.fourcc = 0x34325241; // DRM_FORMAT_ARGB8888 'AR24'
    f.modifier = HCW_MOD_INVALID;
    f.n_planes = 1;
    f.flags = HCW_FRAME_FDS;
    f.seq = 1234;
    f.time_ns = 99;
    f.planes[0].stride = 5120;
    expect_hex("frame", &f,
               "020000005800000007000000000500002003000041523234ffffffffffffff000100000001000000"
               "d2040000000000006300000000000000000000000014000000000000000000000000000000000000"
               "0000000000000000");

    struct hcw_touch t = { 0 };
    hcw_header_init(&t.h, HCW_TOUCH, sizeof t);
    t.kind = HCW_TOUCH_MOTION;
    t.id = 1;
    t.x = 12.5f;
    t.y = 799.0f;
    t.time_ns = 42;
    expect_hex("touch", &t, "080000002000000001000000010000000000484100c047442a00000000000000");

    static struct hcw_builder b;
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv)) {
        perror("socketpair");
        return 1;
    }
    if (hcw_send_state(sv[0], &b, HCW_STATE_ERROR, 500, "Home", "connection refused")) {
        perror("send state");
        return 1;
    }
    expect_hex("state", b.data,
               "0e0000003000000002000000f40100000400000012000000486f6d65636f6e6e656374696f6e20"
               "726566757365640000");

    {
        // guard, built by hand, then walked
        static uint8_t gbuf[64];
        struct hcw_guard *g = (struct hcw_guard *)gbuf;
        uint8_t *p = gbuf + sizeof *g;
        uint32_t l1 = 13, l2 = 2;
        hcw_header_init(&g->h, HCW_GUARD, 40);
        g->n_paths = 2;
        memcpy(p, &l1, 4);
        memcpy(p + 4, "/dashboard-3d", 13);
        memcpy(p + 20, &l2, 4);
        memcpy(p + 24, "/x", 2);
        expect_hex("guard", gbuf, "1100000028000000020000000d0000002f64617368626f6172642d3364000000020000002f780000");
        unsigned total = 0;
        int count = hcw_guard_paths(g, count_path, &total);
        if (count != 2 || total != 15) {
            printf("FAIL guard walk count=%d total=%u\n", count, total);
            failures++;
        }
    }
    if (hcw_send_confirm(sv[0], &b, 3, HCW_CONFIRM_HEAVY_VIEW, "/dashboard-3d/0")) {
        perror("send confirm");
        return 1;
    }
    expect_hex("confirm", b.data, "0f0000002400000003000000000000000f0000002f64617368626f6172642d33642f3000");
    // fd passing: a memfd as the frame's plane.
    static uint8_t buf[HCW_MAX_MESSAGE];
    int fds[HCW_MAX_PLANES];
    unsigned nfds;
    ssize_t n = hcw_recv(sv[1], buf, sizeof buf, fds, &nfds);
    if (n != 48 || nfds != 0) {
        printf("FAIL state recv n=%zd nfds=%u\n", n, nfds);
        failures++;
    }
    n = hcw_recv(sv[1], buf, sizeof buf, fds, &nfds);
    if (n != 36 || nfds != 0 || ((struct hcw_header *)buf)->type != HCW_CONFIRM) {
        printf("FAIL confirm recv n=%zd nfds=%u\n", n, nfds);
        failures++;
    }
    int mfd = memfd_create("plane", MFD_CLOEXEC);
    if (mfd < 0 || write(mfd, "pixels", 6) != 6) {
        perror("memfd");
        return 1;
    }
    if (hcw_send(sv[0], &f, &mfd, 1)) {
        perror("send frame");
        return 1;
    }
    close(mfd);
    n = hcw_recv(sv[1], buf, sizeof buf, fds, &nfds);
    char got[7] = { 0 };
    if (n != (ssize_t)sizeof f || nfds != 1 || pread(fds[0], got, 6, 0) != 6 || strcmp(got, "pixels")) {
        printf("FAIL frame recv n=%zd nfds=%u\n", n, nfds);
        failures++;
    } else {
        printf("ok   frame with fd over a socketpair\n");
        close(fds[0]);
    }
    close(sv[0]);
    n = hcw_recv(sv[1], buf, sizeof buf, fds, &nfds);
    if (n != 0) {
        printf("FAIL eof n=%zd\n", n);
        failures++;
    }
    return failures ? 1 : 0;
}
