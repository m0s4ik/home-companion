# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Builds home-companion-web natively on the device (or in an arm64 root with libwpewebkit-2.0-dev).

PKGS := wpe-webkit-2.0 wpe-platform-2.0 gio-unix-2.0 gbm egl glesv2 libdrm libpng
CFLAGS ?= -O2 -g
CFLAGS += -std=gnu11 -Wall -Wextra -Wno-unused-parameter $(shell pkg-config --cflags $(PKGS))
LDLIBS += $(shell pkg-config --libs $(PKGS)) -lm

SRCS := main.c hc-platform.c snapshot.c
OBJS := $(SRCS:.c=.o)

home-companion-web: $(OBJS)
	$(CC) $(LDFLAGS) -o $@ $(OBJS) $(LDLIBS)

%.o: %.c hc-platform.h protocol.h snapshot.h
	$(CC) $(CFLAGS) -c -o $@ $<

clean:
	rm -f home-companion-web $(OBJS)

.PHONY: clean
