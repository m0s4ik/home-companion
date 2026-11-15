// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

// home-companion-web: Home Assistant's frontend in WPE WebKit, rendered off screen. Each frame is a dma-buf for the
// body (home-companion) to show. See README.md.
//
// With --socket PATH the helper listens on the protocol's socket (web/PROTOCOL.md) and the body takes the
// frames. While no body is connected (and without --socket), a local consumer stands in for it: it
// "presents" each frame at once (paced at 60 Hz) and releases the previous buffer. Control is also line
// based on standard input (show, hide, load, png, ...).

#define _GNU_SOURCE // accept4

#include "hc-platform.h"
#include "protocol.h"
#include "snapshot.h"

#include <drm_fourcc.h>
#include <fcntl.h>
#include <gio/gunixinputstream.h>
#include <glib-unix.h>
#include <dirent.h>
#include <math.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <wpe/webkit.h>

typedef struct {
    // options
    char *url;
    char *data_dir;
    char *cache_dir;
    char *render_node;
    char *primary_node;
    char *theme;
    char *png_path;
    double png_delay;
    double scale;
    int width, height; // physical pixels
    gboolean token_stdin;
    gboolean start_hidden;
    int memory_limit_mb;
    char *socket_path;

    // state
    gint64 t0;
    GMainLoop *loop;
    WPEDisplay *display;
    WebKitWebView *web_view;
    HcView *view;
    char *token_json; // never logged
    GDataInputStream *input;

    guint32 shown_id;     // the latest frame's buffer (held: re-sent to a body that connects)
    HcFrame shown_frame; // its description (fds borrowed from the held buffer)
    guint frame_done_source;
    gint64 last_frame_done;

    guint frames, frames_total;
    gboolean ready;
    gboolean drawn_logged;
    guint png_source;
    guint retry_source;
    guint retry_delay_s;

    // the body (socket mode)
    int listen_fd;
    int conn_fd;        // -1 = no body: the local consumer presents
    guint conn_source;
    GHashTable *sent;      // buffer ids whose fds the body has (this connection)
    GHashTable *body_held; // buffer ids sent to the body and not released yet
    guint64 seq;           // frame counter
    guint64 pending_seq;   // seq of the frame WebKit waits on (0 = none sent to the body)
    GHashTable *touches;   // touch id -> TouchPoint
    WebKitOptionMenu *menu;
    guint32 menu_id;
    GArray *paused;        // pids of the web processes stopped by "pause" (pid_t)
    guint drain_source;    // while paused: wakes them up now and then to take Home Assistant's updates
    gboolean draining;     // they run right now (a drain)
    enum hcw_load_state load_state;
    guint32 progress_milli;
    char *error_message;

    // guarded (heavy) views: path prefixes from the body, the open confirmation
    WebKitUserContentManager *ucm;
    WebKitUserScript *guard_list_script;
    char *guard_list_js; // JSON array of the prefixes
    guint32 confirm_id;
    char *confirm_path; // the path waiting for the body's answer
} App;

typedef struct {
    double x, y; // view pixels
} TouchPoint;

static App app = {
    .theme = NULL,
    .png_delay = 3.0,
    .scale = 1.25,
    .width = 1280,
    .height = 800,
    .listen_fd = -1,
    .conn_fd = -1,
};

static struct hcw_builder builder; // variable messages (64 KiB, not on the stack)

static void say(const char *format, ...) G_GNUC_PRINTF(1, 2);
static void say(const char *format, ...)
{
    va_list args;
    va_start(args, format);
    char *text = g_strdup_vprintf(format, args);
    va_end(args);
    printf("[%8.3f] %s\n", (double)(g_get_monotonic_time() - app.t0) / G_USEC_PER_SEC, text);
    fflush(stdout);
    g_free(text);
}

static const char *fourcc_name(guint32 fourcc, char out[5])
{
    for (int i = 0; i < 4; i++)
        out[i] = (char)((fourcc >> (8 * i)) & 0xff);
    out[4] = 0;
    return out;
}

// ------------------------------------------------------------------- frames: to the body or the local consumer

static void take_png(const char *path)
{
    if (!app.shown_id || !hc_view_lookup(app.view, app.shown_id)) {
        say("png: no frame to save yet");
        return;
    }
    GError *error = NULL;
    gint64 t = g_get_monotonic_time();
    if (hc_snapshot_png(app.display, &app.shown_frame, path, &error))
        say("png: frame %u saved to %s (%.0f ms)", app.shown_id, path, (double)(g_get_monotonic_time() - t) / 1000);
    else {
        say("png: %s", error->message);
        g_error_free(error);
    }
}

static gboolean frame_done_cb(gpointer data)
{
    (void)data;
    app.frame_done_source = 0;
    app.last_frame_done = g_get_monotonic_time();
    hc_view_frame_done(app.view);
    return G_SOURCE_REMOVE;
}

static void schedule_local_frame_done(void)
{
    // Present: let WebKit render the next frame, at most 60 times a second.
    gint64 next = app.last_frame_done + G_USEC_PER_SEC / 60;
    gint64 now = g_get_monotonic_time();
    if (!app.frame_done_source)
        app.frame_done_source = g_timeout_add(next > now ? (guint)((next - now) / 1000) : 0, frame_done_cb, NULL);
}

static gboolean body_connected(void)
{
    return app.conn_fd >= 0;
}

static void body_disconnect(const char *why);
static void body_send_frame(const HcFrame *frame, gboolean webkit_waits);

static void on_frame(HcView *view, const HcFrame *frame, gpointer user_data)
{
    (void)user_data;
    char name[5];
    if (!app.frames_total) {
        say("first frame: %dx%d %s modifier %#" G_GINT64_MODIFIER "x, %u plane(s)", frame->width, frame->height,
            fourcc_name(frame->fourcc, name), frame->modifier, frame->n_planes);
    }
    if (frame->first_time) {
        GString *planes = g_string_new(NULL);
        for (guint32 i = 0; i < frame->n_planes; i++)
            g_string_append_printf(planes, " [fd %d offset %u stride %u]", frame->fds[i], frame->offsets[i],
                                   frame->strides[i]);
        say("buffer %u new: %dx%d %s modifier %#" G_GINT64_MODIFIER "x inode %" G_GUINT64_FORMAT "%s", frame->id,
            frame->width, frame->height, fourcc_name(frame->fourcc, name), frame->modifier, frame->inode,
            planes->str);
        g_string_free(planes, TRUE);
    }
    if (app.ready && !app.drawn_logged) {
        app.drawn_logged = TRUE;
        say("dashboard drawn (first frame after ready)");
    }
    app.frames++;
    app.frames_total++;

    if (body_connected()) {
        // The body releases the previous buffer after its flip and answers `presented`.
        app.shown_id = frame->id;
        app.shown_frame = *frame;
        body_send_frame(frame, TRUE);
        return;
    }
    // Local consumer: the previous buffer is no longer "on screen": give it back.
    if (app.shown_id && app.shown_id != frame->id)
        hc_view_release(view, app.shown_id);
    app.shown_id = frame->id;
    app.shown_frame = *frame;
    schedule_local_frame_done();
}

static void body_send_drop(guint32 id);

static void on_drop(HcView *view, guint32 id, gpointer user_data)
{
    (void)view;
    (void)user_data;
    say("buffer %u dropped", id);
    if (app.shown_id == id)
        app.shown_id = 0;
    body_send_drop(id);
}

static void on_ring(HcView *view, const guint32 *ids, guint n_ids, gpointer user_data)
{
    (void)view;
    (void)user_data;
    GString *list = g_string_new(NULL);
    for (guint i = 0; i < n_ids; i++)
        g_string_append_printf(list, " %u", ids[i]);
    say("WebKit's buffer ring: %u buffer(s):%s", n_ids, list->str);
    g_string_free(list, TRUE);
}

static const HcFrameSink frame_sink = {on_frame, on_drop, on_ring};

static gboolean fps_cb(gpointer data)
{
    (void)data;
    if (app.frames)
        say("fps %u", app.frames);
    app.frames = 0;
    return G_SOURCE_CONTINUE;
}

// -------------------------------------------------------------------------------------- page and login

// A JavaScript string literal for `text` (JSON-compatible, safe inside a script).
static char *js_string(const char *text)
{
    GString *out = g_string_new("\"");
    for (const unsigned char *p = (const unsigned char *)text; *p; p++) {
        if (*p == '"' || *p == '\\')
            g_string_append_printf(out, "\\%c", *p);
        else if (*p < 0x20 || *p == '<' || *p == '>')
            g_string_append_printf(out, "\\u%04x", *p);
        else if (p[0] == 0xe2 && p[1] == 0x80 && (p[2] == 0xa8 || p[2] == 0xa9)) {
            g_string_append_printf(out, "\\u%04x", p[2] == 0xa8 ? 0x2028 : 0x2029);
            p += 2;
        } else
            g_string_append_c(out, (char)*p);
    }
    g_string_append_c(out, '"');
    return g_string_free(out, FALSE);
}

// Runs in the "hc" script world (not visible to the page's own scripts) at document start on the Home
// Assistant origin: puts the token in localStorage if the frontend has none (first start, or after it
// logged out), sets the theme, and keeps the connection while the view is hidden: the frontend closes it
// after 5 minutes hidden ("Automatically close connection" in the HA profile, on by default), and shown
// again it said "Connection lost" and drew the whole dashboard again.
static char *login_script(const char *origin)
{
    char *js_origin = js_string(origin);
    char *js_token = app.token_json ? js_string(app.token_json) : g_strdup("null");
    const char *theme = NULL;
    if (g_strcmp0(app.theme, "dark") == 0)
        theme = "{\"theme\":\"default\",\"dark\":true}";
    else if (g_strcmp0(app.theme, "light") == 0)
        theme = "{\"theme\":\"default\",\"dark\":false}";
    char *js_theme = theme ? js_string(theme) : g_strdup("null");
    char *script = g_strdup_printf("(function () {\n"
                                   "  if (location.origin !== %s) return;\n"
                                   "  try {\n"
                                   "    const token = %s, theme = %s;\n"
                                   "    if (token && !localStorage.getItem('hassTokens'))\n"
                                   "      localStorage.setItem('hassTokens', token);\n"
                                   "    if (theme) localStorage.setItem('selectedTheme', theme);\n"
                                   "    localStorage.setItem('suspendWhenHidden', 'false');\n"
                                   "  } catch (e) {}\n"
                                   "})();\n",
                                   js_origin, js_token, js_theme);
    // The token stays only in the script (needed again if the frontend logs out).
    g_free(js_origin);
    g_free(js_token);
    g_free(js_theme);
    return script;
}

// Tells the helper when the page has its content in the DOM: a Lovelace view with its cards, or any other
// panel (profile, settings) once it has rendered.
static const char ready_script[] =
    "(function () {\n"
    "  const t0 = performance.now();\n"
    "  const post = (m) => window.webkit.messageHandlers.hc.postMessage(m);\n"
    "  // Elements under root, shadow trees included (stops counting at 1000).\n"
    "  const deep = (root, n) => {\n"
    "    for (const e of root.querySelectorAll('*')) {\n"
    "      if (++n > 1000) break;\n"
    "      if (e.shadowRoot) n = deep(e.shadowRoot, n);\n"
    "    }\n"
    "    return n;\n"
    "  };\n"
    "  (function poll() {\n"
    "    let ok = false;\n"
    "    try {\n"
    "      const ha = document.querySelector('home-assistant');\n"
    "      const main = ha.shadowRoot.querySelector('home-assistant-main').shadowRoot;\n"
    "      const lovelace = main.querySelector('ha-panel-lovelace');\n"
    "      if (lovelace) {\n"
    "        const v = lovelace.shadowRoot.querySelector('hui-root').shadowRoot\n"
    "          .querySelector('hui-sections-view, hui-masonry-view, hui-panel-view, hui-sidebar-view');\n"
    "        ok = !!v && (v.shadowRoot ? v.shadowRoot.querySelectorAll('*').length > 20 : true);\n"
    "      } else {\n"
    "        const resolver = main.querySelector('partial-panel-resolver');\n"
    "        const panel = resolver && resolver.lastElementChild;\n"
    "        ok = !!panel && deep(panel.shadowRoot || panel, 0) > 150;\n"
    "      }\n"
    "    } catch (e) {}\n"
    "    if (ok) post('ready ' + Math.round(performance.now()));\n"
    "    else if (performance.now() - t0 < 90000) setTimeout(poll, 100);\n"
    "    else post('timeout');\n"
    "  })();\n"
    "})();\n";

// Runs in the page's own world at document start: Home Assistant navigates between views with
// history.pushState (then a "location-changed" event); a path under one of the guarded prefixes is not
// opened but sent to the helper ("hcGuard"), which asks the body; when the user confirms, the helper
// calls __hcGuard.open(path) and the view opens. The list comes from the body (`guard`).
static const char guard_script_fmt[] =
    "(function () {\n"
    "  if (location.origin !== %s) return;\n"
    "  let guarded = [];\n"
    "  const once = new Set();\n"
    "  const pathOf = (u) => { try { return new URL(u, location.href).pathname; } catch (e) { return ''; } };\n"
    "  const heavy = (p) => guarded.some((g) => g && (p === g || p.startsWith(g.endsWith('/') ? g : g + '/')));\n"
    "  const wrap = (orig) => function (state, title, url) {\n"
    "    if (url != null) {\n"
    "      const p = pathOf(url);\n"
    "      if (p !== location.pathname && heavy(p) && !once.delete(p)) {\n"
    "        const u = new URL(url, location.href);\n"
    "        window.webkit.messageHandlers.hcGuard.postMessage(u.pathname + u.search);\n"
    "        return;\n"
    "      }\n"
    "    }\n"
    "    return orig.call(history, state, title, url);\n"
    "  };\n"
    "  history.pushState = wrap(history.pushState);\n"
    "  history.replaceState = wrap(history.replaceState);\n"
    "  Object.defineProperty(window, '__hcGuard', { value: Object.freeze({\n"
    "    set(list) { guarded = Array.isArray(list) ? list.filter((g) => typeof g === 'string') : []; },\n"
    "    open(u) {\n"
    "      once.add(pathOf(u));\n"
    "      history.pushState(null, '', u);\n"
    "      window.dispatchEvent(new CustomEvent('location-changed', { detail: { replace: false } }));\n"
    "    },\n"
    "  }) });\n"
    "})();\n";

static void run_main_world(const char *js)
{
    if (app.web_view)
        webkit_web_view_evaluate_javascript(app.web_view, js, -1, NULL, NULL, NULL, NULL, NULL);
}

static void add_guard_path(const char *path, uint32_t len, void *data)
{
    char *p = g_strndup(path, len);
    char *js = js_string(p);
    GString *out = data;
    g_string_append_printf(out, "%s%s", out->len > 1 ? "," : "", js);
    g_free(js);
    g_free(p);
}

// `guard` from the body: the list goes into a user script (kept across reloads) and into the page now.
static void on_guard(const struct hcw_guard *m)
{
    GString *list = g_string_new("[");
    int n = hcw_guard_paths(m, add_guard_path, list);
    g_string_append_c(list, ']');
    if (n < 0) {
        say("guard: bad message (ignored)");
        g_string_free(list, TRUE);
        return;
    }
    g_free(app.guard_list_js);
    app.guard_list_js = g_string_free(list, FALSE);
    say("guard: %d path prefix(es)", n);
    if (!app.ucm)
        return;
    if (app.guard_list_script) {
        webkit_user_content_manager_remove_script(app.ucm, app.guard_list_script);
        g_clear_pointer(&app.guard_list_script, webkit_user_script_unref);
    }
    char *js = g_strdup_printf("window.__hcGuard && window.__hcGuard.set(%s);", app.guard_list_js);
    app.guard_list_script = webkit_user_script_new(js, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                                   WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, NULL, NULL);
    webkit_user_content_manager_add_script(app.ucm, app.guard_list_script);
    run_main_world(js);
    g_free(js);
}

static void on_guard_message(WebKitUserContentManager *manager, JSCValue *value, gpointer data)
{
    (void)manager;
    (void)data;
    char *path = jsc_value_to_string(value);
    if (!body_connected()) {
        say("guarded view %s: no body to ask, not opened", path);
        g_free(path);
        return;
    }
    g_free(app.confirm_path);
    app.confirm_path = path;
    app.confirm_id++;
    say("guarded view %s: asking the body (confirm %u)", path, app.confirm_id);
    if (hcw_send_confirm(app.conn_fd, &builder, app.confirm_id, HCW_CONFIRM_HEAVY_VIEW, path) < 0) {
        char *why = g_strdup_printf("send confirm: %s", g_strerror(errno));
        body_disconnect(why);
        g_free(why);
    }
}

static void on_confirm_result(guint32 id, gboolean accepted)
{
    if (id != app.confirm_id || !app.confirm_path) {
        say("confirm-result %u: not asked (ignored)", id);
        return;
    }
    say("guarded view %s: %s", app.confirm_path, accepted ? "opened" : "not opened");
    if (accepted) {
        char *js_path = js_string(app.confirm_path);
        char *js = g_strdup_printf("window.__hcGuard && window.__hcGuard.open(%s);", js_path);
        run_main_world(js);
        g_free(js);
        g_free(js_path);
    }
    g_clear_pointer(&app.confirm_path, g_free);
}

// The body asks for another device scale (its `hello`): the view is laid out again at that scale.
static void apply_scale(double scale)
{
    if (scale < 0.5 || scale > 4 || fabs(scale - app.scale) < 0.001 || !app.view)
        return;
    WPEToplevel *toplevel = wpe_view_get_toplevel(WPE_VIEW(app.view));
    if (!toplevel)
        return;
    app.scale = scale;
    hc_toplevel_configure(HC_TOPLEVEL(toplevel), (int)(app.width / app.scale + 0.5),
                           (int)(app.height / app.scale + 0.5), app.scale);
    say("scale %.2f from the body", app.scale);
}

static void send_state(void);

static void set_state(enum hcw_load_state state, guint32 progress_milli, const char *message)
{
    app.load_state = state;
    app.progress_milli = progress_milli;
    g_free(app.error_message);
    app.error_message = g_strdup(message ? message : "");
    send_state();
}

static gboolean png_cb(gpointer data)
{
    (void)data;
    app.png_source = 0;
    take_png(app.png_path);
    g_clear_pointer(&app.png_path, g_free); // once
    return G_SOURCE_REMOVE;
}

static void on_script_message(WebKitUserContentManager *manager, JSCValue *value, gpointer data)
{
    (void)manager;
    (void)data;
    char *text = jsc_value_to_string(value);
    if (g_str_has_prefix(text, "ready")) {
        app.ready = TRUE;
        app.drawn_logged = FALSE;
        say("dashboard ready (content in the DOM; %s ms after navigation start)", text + 6);
        set_state(HCW_STATE_READY, 1000, NULL);
        if (app.png_path && !app.png_source)
            app.png_source = g_timeout_add((guint)(app.png_delay * 1000), png_cb, NULL);
    } else {
        say("page: %s", text);
        if (strcmp(text, "timeout") == 0)
            set_state(HCW_STATE_ERROR, 1000, "the page did not show its content within 90 s");
    }
    g_free(text);
}

static void load(const char *url)
{
    app.ready = FALSE;
    app.drawn_logged = FALSE;
    say("load %s", url);
    webkit_web_view_load_uri(app.web_view, url);
}

static gboolean retry_cb(gpointer data)
{
    (void)data;
    app.retry_source = 0;
    webkit_web_view_reload(app.web_view);
    return G_SOURCE_REMOVE;
}

static void on_load_changed(WebKitWebView *web_view, WebKitLoadEvent event, gpointer data)
{
    (void)web_view;
    (void)data;
    static const char *names[] = {"started", "redirected", "committed", "finished"};
    say("load %s", names[event]);
    if (event == WEBKIT_LOAD_STARTED) {
        app.ready = FALSE;
        set_state(HCW_STATE_LOADING, 0, NULL);
    }
    if (event == WEBKIT_LOAD_FINISHED)
        app.retry_delay_s = 0;
}

static void on_progress(WebKitWebView *web_view, GParamSpec *pspec, gpointer data)
{
    (void)pspec;
    (void)data;
    if (app.load_state != HCW_STATE_LOADING)
        return;
    // The page's own progress; the "ready" state comes from the script once the content is there.
    guint32 p = (guint32)(webkit_web_view_get_estimated_load_progress(web_view) * 900);
    if (p >= app.progress_milli + 100 || (p == 900 && app.progress_milli != 900))
        set_state(HCW_STATE_LOADING, p, NULL);
}

static void on_title(WebKitWebView *web_view, GParamSpec *pspec, gpointer data)
{
    (void)web_view;
    (void)pspec;
    (void)data;
    send_state();
}

static gboolean on_load_failed(WebKitWebView *web_view, WebKitLoadEvent event, char *uri, GError *error,
                               gpointer data)
{
    (void)web_view;
    (void)event;
    (void)data;
    if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_CANCELLED))
        return FALSE;
    app.retry_delay_s = app.retry_delay_s ? MIN(app.retry_delay_s * 2, 60) : 2;
    say("load failed (%s): %s; retry in %u s", uri, error->message, app.retry_delay_s);
    set_state(HCW_STATE_ERROR, 0, error->message);
    if (!app.retry_source)
        app.retry_source = g_timeout_add_seconds(app.retry_delay_s, retry_cb, NULL);
    return FALSE;
}

static void on_web_process_terminated(WebKitWebView *web_view, WebKitWebProcessTerminationReason reason,
                                      gpointer data)
{
    (void)data;
    say("web process terminated (reason %d); reloading", reason);
    app.ready = FALSE;
    set_state(HCW_STATE_LOADING, 0, "the web process stopped; reloading");
    webkit_web_view_reload(web_view);
}

// ------------------------------------------------------------------------------------------- control

// The web processes (WPEWebProcess) started by this helper: its descendants with that name (they run
// under bwrap, so not direct children).
static GArray *web_processes(void)
{
    GArray *pid = g_array_new(FALSE, FALSE, sizeof(pid_t)), *ppid = g_array_new(FALSE, FALSE, sizeof(pid_t));
    GPtrArray *comm = g_ptr_array_new_with_free_func(g_free);
    DIR *d = opendir("/proc");
    struct dirent *e;
    while (d && (e = readdir(d))) {
        char *end;
        long p = strtol(e->d_name, &end, 10);
        if (*end || p <= 0)
            continue;
        char path[64], line[512];
        snprintf(path, sizeof path, "/proc/%ld/stat", p);
        FILE *f = fopen(path, "r");
        if (!f)
            continue;
        size_t n = fread(line, 1, sizeof line - 1, f);
        fclose(f);
        line[n] = 0;
        // "pid (comm) state ppid ...": comm may hold spaces and parentheses, so take the last ')'.
        char *open = strchr(line, '('), *close = strrchr(line, ')');
        if (!open || !close || close < open)
            continue;
        pid_t pp = (pid_t)strtol(close + 2 + 2, NULL, 10); // ") S <ppid>"
        pid_t pv = (pid_t)p;
        g_array_append_val(pid, pv);
        g_array_append_val(ppid, pp);
        g_ptr_array_add(comm, g_strndup(open + 1, (gsize)(close - open - 1)));
    }
    if (d)
        closedir(d);
    // Walk down from this helper: a process is ours if its parent is.
    GHashTable *ours = g_hash_table_new(NULL, NULL);
    g_hash_table_add(ours, GINT_TO_POINTER(getpid()));
    for (gboolean grew = TRUE; grew;) {
        grew = FALSE;
        for (guint i = 0; i < pid->len; i++) {
            gpointer me = GINT_TO_POINTER(g_array_index(pid, pid_t, i));
            if (!g_hash_table_contains(ours, me)
                && g_hash_table_contains(ours, GINT_TO_POINTER(g_array_index(ppid, pid_t, i)))) {
                g_hash_table_add(ours, me);
                grew = TRUE;
            }
        }
    }
    GArray *web = g_array_new(FALSE, FALSE, sizeof(pid_t));
    for (guint i = 0; i < pid->len; i++)
        if (g_hash_table_contains(ours, GINT_TO_POINTER(g_array_index(pid, pid_t, i)))
            && g_strcmp0(g_ptr_array_index(comm, i), "WPEWebProcess") == 0)
            g_array_append_val(web, g_array_index(pid, pid_t, i));
    g_hash_table_destroy(ours);
    g_array_free(pid, TRUE);
    g_array_free(ppid, TRUE);
    g_ptr_array_free(comm, TRUE);
    return web;
}

// "pause": the web process is stopped (SIGSTOP) while the view is hidden for long, the network process is
// not, so the page's WebSocket to Home Assistant stays open (the network process answers its pings) and
// the dashboard is there at once when shown. Freezing the whole cgroup closed it: Home Assistant drops a
// client that does not answer its pings, and the frontend said "Connection lost" and drew everything again.
// Home Assistant's updates wait for the stopped page, though: after 25 minutes it spent 22 s on them when
// shown, with the old data on screen. So it is let run DRAIN_RUN_MS every DRAIN_EVERY_S to take them.
#define DRAIN_EVERY_S 60
#define DRAIN_RUN_MS 3000

static void signal_paused(int sig)
{
    for (guint i = 0; i < app.paused->len; i++)
        kill(g_array_index(app.paused, pid_t, i), sig);
}

static gboolean drain_tick(gpointer data)
{
    (void)data;
    app.draining = !app.draining;
    signal_paused(app.draining ? SIGCONT : SIGSTOP);
    // Next: stop again after the run, or the next run after the wait.
    app.drain_source = g_timeout_add(app.draining ? DRAIN_RUN_MS : (DRAIN_EVERY_S * 1000 - DRAIN_RUN_MS),
                                     drain_tick, NULL);
    return G_SOURCE_REMOVE;
}

static void pause_web(gboolean pause)
{
    if (pause) {
        if (app.paused->len)
            return;
        GArray *web = web_processes();
        for (guint i = 0; i < web->len; i++) {
            pid_t p = g_array_index(web, pid_t, i);
            if (kill(p, SIGSTOP) == 0)
                g_array_append_val(app.paused, p);
            else
                say("pause: web process %d: %s", (int)p, g_strerror(errno));
        }
        g_array_free(web, TRUE);
        if (app.paused->len) {
            app.draining = FALSE;
            app.drain_source = g_timeout_add(DRAIN_EVERY_S * 1000 - DRAIN_RUN_MS, drain_tick, NULL);
        }
        say("paused (%u web process(es) stopped, %d s of every %d s to take updates)", app.paused->len,
            DRAIN_RUN_MS / 1000, DRAIN_EVERY_S);
    } else if (app.paused->len) {
        if (app.drain_source)
            g_source_remove(app.drain_source);
        app.drain_source = 0;
        app.draining = FALSE;
        signal_paused(SIGCONT);
        say("resumed (%u web process(es))", app.paused->len);
        g_array_set_size(app.paused, 0);
    }
}

static void set_visible(gboolean visible)
{
    if (visible) {
        pause_web(FALSE);
        wpe_view_set_visible(WPE_VIEW(app.view), TRUE);
        wpe_view_focus_in(WPE_VIEW(app.view));
        say("shown");
    } else {
        wpe_view_focus_out(WPE_VIEW(app.view));
        wpe_view_set_visible(WPE_VIEW(app.view), FALSE);
        say("hidden");
    }
}

static void touch(WPEEventType type, guint32 id, double x, double y, guint32 time_ms)
{
    // Body coordinates are physical view pixels; WPE events are logical.
    WPEEvent *event = wpe_event_touch_new(type, WPE_VIEW(app.view), WPE_INPUT_SOURCE_TOUCHSCREEN, time_ms, 0, id,
                                          x / app.scale, y / app.scale);
    wpe_view_event(WPE_VIEW(app.view), event);
    wpe_event_unref(event);
}

// Ends every active touch point (body asked for it, or it went away).
static void touch_cancel_all(guint32 time_ms)
{
    GHashTableIter iter;
    gpointer key, value;
    g_hash_table_iter_init(&iter, app.touches);
    while (g_hash_table_iter_next(&iter, &key, &value)) {
        const TouchPoint *p = value;
        touch(WPE_EVENT_TOUCH_CANCEL, GPOINTER_TO_UINT(key), p->x, p->y, time_ms);
    }
    g_hash_table_remove_all(app.touches);
}

static void read_next_line(void);

static void handle_command(char *line)
{
    g_strstrip(line);
    if (!*line)
        return;
    char *arg = strchr(line, ' ');
    if (arg) {
        *arg++ = 0;
        g_strstrip(arg);
    }
    if (strcmp(line, "show") == 0)
        set_visible(TRUE);
    else if (strcmp(line, "hide") == 0)
        set_visible(FALSE);
    else if (strcmp(line, "load") == 0 && arg && *arg)
        load(arg);
    else if (strcmp(line, "reload") == 0) {
        app.ready = FALSE;
        webkit_web_view_reload(app.web_view);
    } else if (strcmp(line, "back") == 0)
        webkit_web_view_go_back(app.web_view);
    else if (strcmp(line, "png") == 0 && arg && *arg)
        take_png(arg);
    else if (strcmp(line, "tap") == 0 && arg) {
        double x, y;
        if (sscanf(arg, "%lf %lf", &x, &y) == 2) {
            guint32 t = (guint32)(g_get_monotonic_time() / 1000);
            touch(WPE_EVENT_TOUCH_DOWN, 100, x, y, t);
            touch(WPE_EVENT_TOUCH_UP, 100, x, y, t);
            say("tap %.0f %.0f", x, y);
        }
    } else if (strcmp(line, "quit") == 0)
        g_main_loop_quit(app.loop);
    else
        say("unknown command: %s", line);
}

static void on_line(GObject *source, GAsyncResult *result, gpointer data)
{
    (void)data;
    GError *error = NULL;
    char *line = g_data_input_stream_read_line_finish_utf8(G_DATA_INPUT_STREAM(source), result, NULL, &error);
    if (!line) {
        if (error) {
            say("stdin: %s", error->message);
            g_error_free(error);
        }
        if (app.listen_fd >= 0) {
            // With a socket the body is in charge: standard input is optional.
            say("stdin closed");
            return;
        }
        say("stdin closed: quitting");
        g_main_loop_quit(app.loop);
        return;
    }
    handle_command(line);
    g_free(line);
    read_next_line();
}

static void read_next_line(void)
{
    g_data_input_stream_read_line_async(app.input, G_PRIORITY_DEFAULT, NULL, on_line, NULL);
}

static gboolean on_signal(gpointer data)
{
    (void)data;
    g_main_loop_quit(app.loop);
    return G_SOURCE_REMOVE;
}

// ------------------------------------------------------------------------------- the body (socket mode)

static gboolean body_send(const void *msg, const int *fds, unsigned nfds)
{
    if (!body_connected())
        return FALSE;
    if (hcw_send(app.conn_fd, msg, fds, nfds) == 0)
        return TRUE;
    // A body that does not read (full socket buffer) is as good as gone: it reconnects.
    char *why = g_strdup_printf("send: %s", g_strerror(errno));
    body_disconnect(why);
    g_free(why);
    return FALSE;
}

static void send_state(void)
{
    if (!body_connected())
        return;
    const char *title = app.web_view ? webkit_web_view_get_title(app.web_view) : NULL;
    if (hcw_send_state(app.conn_fd, &builder, app.load_state, app.progress_milli, title ? title : "",
                        app.error_message ? app.error_message : "") < 0) {
        char *why = g_strdup_printf("send state: %s", g_strerror(errno));
        body_disconnect(why);
        g_free(why);
    }
}

// The fds are the WPEBuffer's own (borrowed): the kernel duplicates them into the message, nothing to close.
static void body_send_frame(const HcFrame *frame, gboolean webkit_waits)
{
    struct hcw_frame m = {0};
    hcw_header_init(&m.h, HCW_FRAME, sizeof m);
    m.buffer = frame->id;
    m.width = (guint32)frame->width;
    m.height = (guint32)frame->height;
    m.fourcc = frame->fourcc;
    m.modifier = frame->modifier;
    m.n_planes = frame->n_planes;
    m.seq = ++app.seq;
    m.time_ns = (guint64)g_get_monotonic_time() * 1000;
    for (guint32 i = 0; i < frame->n_planes; i++) {
        m.planes[i].offset = frame->offsets[i];
        m.planes[i].stride = frame->strides[i];
    }
    gboolean with_fds = !g_hash_table_contains(app.sent, GUINT_TO_POINTER(frame->id));
    if (with_fds)
        m.flags |= HCW_FRAME_FDS;
    if (webkit_waits)
        app.pending_seq = m.seq;
    g_hash_table_add(app.body_held, GUINT_TO_POINTER(frame->id));
    if (body_send(&m, frame->fds, with_fds ? frame->n_planes : 0) && with_fds)
        g_hash_table_add(app.sent, GUINT_TO_POINTER(frame->id));
}

static void body_send_drop(guint32 id)
{
    if (!body_connected())
        return;
    g_hash_table_remove(app.body_held, GUINT_TO_POINTER(id));
    if (!g_hash_table_remove(app.sent, GUINT_TO_POINTER(id)))
        return; // the body never had it
    struct hcw_buffer_msg m = {0};
    hcw_header_init(&m.h, HCW_DROP, sizeof m);
    m.buffer = id;
    body_send(&m, NULL, 0);
}

static void menu_forget(void)
{
    if (!app.menu)
        return;
    g_signal_handlers_disconnect_by_data(app.menu, &app.menu);
    g_clear_object(&app.menu);
}

static void on_menu_closed(WebKitOptionMenu *menu, gpointer data)
{
    (void)menu;
    (void)data;
    say("menu %u closed by the page", app.menu_id);
    menu_forget();
}

// A <select> opened by the page: the body draws the list natively and answers with menu-result.
static gboolean on_show_option_menu(WebKitWebView *web_view, WebKitOptionMenu *menu, WebKitRectangle *rect,
                                    gpointer data)
{
    (void)web_view;
    (void)data;
    if (!body_connected())
        return FALSE;
    if (app.menu) {
        webkit_option_menu_close(app.menu);
        menu_forget();
    }
    struct hcw_menu *m = hcw_build_begin(&builder, HCW_MENU, sizeof *m);
    m->menu = ++app.menu_id;
    m->selected = -1;
    m->x = (int32_t)(rect->x * app.scale);
    m->y = (int32_t)(rect->y * app.scale);
    m->width = (int32_t)(rect->width * app.scale);
    m->height = (int32_t)(rect->height * app.scale);
    guint n = webkit_option_menu_get_n_items(menu);
    for (guint i = 0; i < n; i++) {
        WebKitOptionMenuItem *item = webkit_option_menu_get_item(menu, i);
        guint32 flags = (webkit_option_menu_item_is_enabled(item) ? HCW_MENU_ITEM_ENABLED : 0) |
                        (webkit_option_menu_item_is_group_label(item) ? HCW_MENU_ITEM_GROUP : 0);
        if (webkit_option_menu_item_is_selected(item) && m->selected < 0)
            m->selected = (int32_t)i;
        const char *label = webkit_option_menu_item_get_label(item);
        if (hcw_build_menu_item(&builder, flags, label ? label : "") < 0) {
            say("menu: too many items (%u), list cut at %u", n, i);
            break;
        }
    }
    say("menu %u: %u item(s), selected %d", m->menu, m->n_items, m->selected);
    app.menu = g_object_ref(menu);
    g_signal_connect(menu, "close", G_CALLBACK(on_menu_closed), &app.menu);
    if (hcw_build_send(app.conn_fd, &builder) < 0) {
        webkit_option_menu_close(menu);
        menu_forget();
        char *why = g_strdup_printf("send menu: %s", g_strerror(errno));
        body_disconnect(why);
        g_free(why);
    }
    return TRUE;
}

static void on_menu_result(guint32 id, int32_t index)
{
    if (!app.menu || id != app.menu_id) {
        say("menu-result for menu %u: not open (ignored)", id);
        return;
    }
    WebKitOptionMenu *menu = g_object_ref(app.menu);
    menu_forget();
    if (index >= 0 && (guint)index < webkit_option_menu_get_n_items(menu)) {
        say("menu %u: item %d chosen", id, index);
        webkit_option_menu_activate_item(menu, (guint)index); // closes the menu
    } else {
        say("menu %u: cancelled", id);
        webkit_option_menu_close(menu);
    }
    g_object_unref(menu);
}

static void on_body_touch(const struct hcw_touch *m)
{
    guint32 t = (guint32)(m->time_ns / 1000000);
    guint32 id = (guint32)m->id;
    switch (m->kind) {
    case HCW_TOUCH_DOWN:
    case HCW_TOUCH_MOTION: {
        TouchPoint *p = g_hash_table_lookup(app.touches, GUINT_TO_POINTER(id));
        gboolean down = m->kind == HCW_TOUCH_DOWN || !p;
        if (!p) {
            p = g_new(TouchPoint, 1);
            g_hash_table_insert(app.touches, GUINT_TO_POINTER(id), p);
        }
        p->x = m->x;
        p->y = m->y;
        touch(down ? WPE_EVENT_TOUCH_DOWN : WPE_EVENT_TOUCH_MOVE, id, m->x, m->y, t);
        break;
    }
    case HCW_TOUCH_UP:
        if (g_hash_table_remove(app.touches, GUINT_TO_POINTER(id)))
            touch(WPE_EVENT_TOUCH_UP, id, m->x, m->y, t);
        break;
    case HCW_TOUCH_CANCEL:
        touch_cancel_all(t);
        break;
    default:
        break;
    }
}

static void on_body_message(const void *buf, size_t len)
{
    const struct hcw_header *h = buf;
#define BODY_MSG(type) (len >= sizeof(type) ? (const type *)buf : NULL)
    switch (h->type) {
    case HCW_HELLO: {
        const struct hcw_hello *m = BODY_MSG(struct hcw_hello);
        if (m) {
            say("body hello: version %u, view %ux%u at scale %.2f", m->version, m->width, m->height,
                m->scale_milli / 1000.0);
            if (m->scale_milli)
                apply_scale(m->scale_milli / 1000.0);
        }
        break;
    }
    case HCW_PRESENTED: {
        const struct hcw_presented *m = BODY_MSG(struct hcw_presented);
        // Every frame is answered once, shown or skipped: WebKit may render the next one.
        if (m && m->seq == app.pending_seq) {
            app.pending_seq = 0;
            app.last_frame_done = g_get_monotonic_time();
            hc_view_frame_done(app.view);
        }
        break;
    }
    case HCW_RELEASE: {
        const struct hcw_buffer_msg *m = BODY_MSG(struct hcw_buffer_msg);
        if (m && g_hash_table_remove(app.body_held, GUINT_TO_POINTER(m->buffer))) {
            if (app.shown_id == m->buffer)
                app.shown_id = 0;
            hc_view_release(app.view, m->buffer);
        }
        break;
    }
    case HCW_SHOW:
        set_visible(TRUE);
        break;
    case HCW_HIDE:
        set_visible(FALSE);
        break;
    case HCW_PAUSE:
        pause_web(TRUE);
        break;
    case HCW_RESUME:
        pause_web(FALSE);
        break;
    case HCW_TOUCH: {
        const struct hcw_touch *m = BODY_MSG(struct hcw_touch);
        if (m)
            on_body_touch(m);
        break;
    }
    case HCW_MENU_RESULT: {
        const struct hcw_menu_result *m = BODY_MSG(struct hcw_menu_result);
        if (m)
            on_menu_result(m->menu, m->index);
        break;
    }
    case HCW_LOAD: {
        const struct hcw_load *m = BODY_MSG(struct hcw_load);
        const char *url = m ? hcw_load_url(m) : NULL;
        if (url) {
            char *copy = g_strndup(url, m->url_len);
            load(copy);
            g_free(copy);
        }
        break;
    }
    case HCW_RELOAD:
        say("reload");
        app.ready = FALSE;
        webkit_web_view_reload(app.web_view);
        break;
    case HCW_BACK:
        say("back");
        webkit_web_view_go_back(app.web_view);
        break;
    case HCW_CONFIRM_RESULT: {
        const struct hcw_confirm_result *m = BODY_MSG(struct hcw_confirm_result);
        if (m)
            on_confirm_result(m->id, m->accepted != 0);
        break;
    }
    case HCW_GUARD: {
        const struct hcw_guard *m = BODY_MSG(struct hcw_guard);
        if (m)
            on_guard(m);
        break;
    }
    default:
        say("body: message type %u skipped", h->type);
        break;
    }
#undef BODY_MSG
}

static gboolean on_body_readable(int fd, GIOCondition condition, gpointer data)
{
    (void)condition;
    (void)data;
    static uint8_t buf[HCW_MAX_MESSAGE];
    for (;;) {
        int fds[HCW_MAX_PLANES];
        unsigned nfds = 0;
        ssize_t n = hcw_recv(fd, buf, sizeof buf, fds, &nfds);
        for (unsigned i = 0; i < nfds; i++) // the body sends no fds
            close(fds[i]);
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
            return G_SOURCE_CONTINUE;
        if (n < 0 && (errno == EPROTO || errno == EMSGSIZE)) {
            say("body: bad message (%s), skipped", g_strerror(errno));
            continue;
        }
        if (n <= 0) {
            app.conn_source = 0; // removed by returning G_SOURCE_REMOVE
            body_disconnect(n == 0 ? "closed by the body" : g_strerror(errno));
            return G_SOURCE_REMOVE;
        }
        on_body_message(buf, (size_t)n);
        if (fd != app.conn_fd) // the message made us disconnect
            return G_SOURCE_REMOVE;
    }
}

// Forgets the body: every buffer it held goes back to WebKit except the latest frame's (kept, so a body
// that reconnects gets it at once), WebKit's waiting frame is completed, the local consumer takes over.
static void body_disconnect(const char *why)
{
    if (!body_connected())
        return;
    say("body disconnected (%s)", why);
    pause_web(FALSE); // a body that went away never says "resume"
    if (app.conn_source)
        g_source_remove(app.conn_source);
    app.conn_source = 0;
    close(app.conn_fd);
    app.conn_fd = -1;

    GHashTableIter iter;
    gpointer key;
    g_hash_table_iter_init(&iter, app.body_held);
    GArray *release = g_array_new(FALSE, FALSE, sizeof(guint32));
    while (g_hash_table_iter_next(&iter, &key, NULL)) {
        guint32 id = GPOINTER_TO_UINT(key);
        if (id != app.shown_id)
            g_array_append_val(release, id);
    }
    g_hash_table_remove_all(app.body_held);
    g_hash_table_remove_all(app.sent);
    for (guint i = 0; i < release->len; i++)
        hc_view_release(app.view, g_array_index(release, guint32, i));
    g_array_free(release, TRUE);

    if (app.menu) {
        webkit_option_menu_close(app.menu);
        menu_forget();
    }
    touch_cancel_all((guint32)(g_get_monotonic_time() / 1000));
    app.pending_seq = 0;
    if (hc_view_has_pending_frame(app.view))
        schedule_local_frame_done();
}

static gboolean on_body_connect(int fd, GIOCondition condition, gpointer data)
{
    (void)condition;
    (void)data;
    int conn = accept4(fd, NULL, NULL, SOCK_CLOEXEC | SOCK_NONBLOCK);
    if (conn < 0) {
        if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)
            say("accept: %s", g_strerror(errno));
        return G_SOURCE_CONTINUE;
    }
    // One body at a time: a new connection replaces the old one (the body restarted).
    body_disconnect("a new body connected");
    // The local consumer's frame (if any) is done now; the body answers the next ones.
    if (app.frame_done_source) {
        g_source_remove(app.frame_done_source);
        frame_done_cb(NULL);
    }
    app.conn_fd = conn;
    app.conn_source = g_unix_fd_add(conn, G_IO_IN | G_IO_HUP | G_IO_ERR, on_body_readable, NULL);
    say("body connected");

    struct hcw_hello hello = {0};
    hcw_header_init(&hello.h, HCW_HELLO, sizeof hello);
    hello.version = HCW_VERSION;
    hello.width = (guint32)app.width;
    hello.height = (guint32)app.height;
    hello.scale_milli = (guint32)(app.scale * 1000 + 0.5);
    if (!body_send(&hello, NULL, 0))
        return G_SOURCE_CONTINUE;
    send_state();
    // The latest frame at once (possibly stale: the page renders a fresh one when shown).
    if (body_connected() && app.shown_id && hc_view_lookup(app.view, app.shown_id))
        body_send_frame(&app.shown_frame, FALSE);
    return G_SOURCE_CONTINUE;
}

static gboolean body_listen(const char *path, GError **error)
{
    struct sockaddr_un addr = {.sun_family = AF_UNIX};
    if (strlen(path) >= sizeof addr.sun_path) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT, "socket path too long: %s", path);
        return FALSE;
    }
    strcpy(addr.sun_path, path);
    int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0) {
        g_set_error(error, G_IO_ERROR, g_io_error_from_errno(errno), "socket: %s", g_strerror(errno));
        return FALSE;
    }
    unlink(path); // a stale socket from an earlier run
    mode_t old = umask(0117); // owner and group (the body's group in the unit) may connect
    int r = bind(fd, (struct sockaddr *)&addr, sizeof addr);
    umask(old);
    if (r < 0 || listen(fd, 2) < 0) {
        g_set_error(error, G_IO_ERROR, g_io_error_from_errno(errno), "%s: %s", path, g_strerror(errno));
        close(fd);
        return FALSE;
    }
    app.listen_fd = fd;
    g_unix_fd_add(fd, G_IO_IN, on_body_connect, NULL);
    say("listening on %s", path);
    return TRUE;
}

// --------------------------------------------------------------------------------------------- setup

static void setup_environment(void)
{
    // freedreno's GMEM tiling loops forever on MSAA render targets with depth (Adreno 5xx) up to Mesa 26.3.0-1;
    // sysmem rendering avoids it. The unit decides: when FD_MESA_DEBUG is set, even empty (mesa-freedreno 26.3.0-2
    // and later carry the fix, see install.sh), it is left as it is; unset, sysmem is the safe default. Set before
    // WebKit starts, so the web (and GPU) processes inherit it. Only here: the body keeps tiling.
    if (!g_getenv("FD_MESA_DEBUG"))
        g_setenv("FD_MESA_DEBUG", "sysmem", TRUE);
}

static char *origin_of(const char *url)
{
    GUri *uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (!uri)
        return NULL;
    char *origin = g_uri_get_port(uri) > 0
                       ? g_strdup_printf("%s://%s:%d", g_uri_get_scheme(uri), g_uri_get_host(uri), g_uri_get_port(uri))
                       : g_strdup_printf("%s://%s", g_uri_get_scheme(uri), g_uri_get_host(uri));
    g_uri_unref(uri);
    return origin;
}

static gboolean create_web_view(GError **error)
{
    char *origin = origin_of(app.url);
    if (!origin) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT, "not a URL: %s", app.url);
        return FALSE;
    }
    GUri *uri = g_uri_parse(app.url, G_URI_FLAGS_NONE, NULL);
    // URL patterns of user scripts match the host, not the port; the script checks the full origin.
    char *pattern = g_strdup_printf("%s://%s/*", g_uri_get_scheme(uri), g_uri_get_host(uri));
    g_uri_unref(uri);
    const char *allow[] = {pattern, NULL};

    WebKitUserContentManager *ucm = webkit_user_content_manager_new();
    g_signal_connect(ucm, "script-message-received::hc", G_CALLBACK(on_script_message), NULL);
    webkit_user_content_manager_register_script_message_handler(ucm, "hc", "hc");
    char *login = login_script(origin);
    WebKitUserScript *script =
        webkit_user_script_new_for_world(login, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                         WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, "hc", allow, NULL);
    webkit_user_content_manager_add_script(ucm, script);
    webkit_user_script_unref(script);
    memset(login, 0, strlen(login));
    g_free(login);
    script = webkit_user_script_new_for_world(ready_script, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                              WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_END, "hc", allow, NULL);
    webkit_user_content_manager_add_script(ucm, script);
    webkit_user_script_unref(script);
    char *js_origin = js_string(origin);
    char *guard = g_strdup_printf(guard_script_fmt, js_origin);
    g_signal_connect(ucm, "script-message-received::hcGuard", G_CALLBACK(on_guard_message), NULL);
    webkit_user_content_manager_register_script_message_handler(ucm, "hcGuard", NULL);
    script = webkit_user_script_new(guard, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                    WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, allow, NULL);
    webkit_user_content_manager_add_script(ucm, script);
    webkit_user_script_unref(script);
    g_free(guard);
    g_free(js_origin);
    app.ucm = ucm;
    g_free(pattern);
    g_free(origin);

    WebKitWebContext *context;
    if (app.memory_limit_mb > 0) {
        WebKitMemoryPressureSettings *pressure = webkit_memory_pressure_settings_new();
        webkit_memory_pressure_settings_set_memory_limit(pressure, (guint)app.memory_limit_mb);
        context = g_object_new(WEBKIT_TYPE_WEB_CONTEXT, "memory-pressure-settings", pressure, NULL);
        webkit_memory_pressure_settings_free(pressure);
    } else
        context = webkit_web_context_new();

    WebKitNetworkSession *session = webkit_network_session_new(app.data_dir, app.cache_dir);
    WebKitSettings *settings =
        webkit_settings_new_with_settings("enable-2d-canvas-acceleration", TRUE, "enable-webgl", TRUE,
                                          "enable-developer-extras", FALSE, NULL);
    WebKitWebsitePolicies *policies = webkit_website_policies_new_with_policies("autoplay", WEBKIT_AUTOPLAY_DENY, NULL);

    app.web_view = g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", app.display, "web-context", context,
                                "network-session", session, "settings", settings, "user-content-manager", ucm,
                                "website-policies", policies, NULL);
    g_object_unref(context);
    g_object_unref(session);
    g_object_unref(settings);
    g_object_unref(policies);
    g_object_unref(ucm);

    WPEView *wpe_view = webkit_web_view_get_wpe_view(app.web_view);
    if (!HC_IS_VIEW(wpe_view)) {
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_FAILED, "WebKit did not use our view");
        return FALSE;
    }
    app.view = HC_VIEW(wpe_view);
    hc_view_set_sink(app.view, &frame_sink, NULL);
    WPEToplevel *toplevel = wpe_view_get_toplevel(wpe_view);
    if (!toplevel) {
        toplevel = wpe_display_create_toplevel(app.display, 1);
        wpe_view_set_toplevel(wpe_view, toplevel);
        g_object_unref(toplevel);
    }
    hc_toplevel_configure(HC_TOPLEVEL(toplevel), (int)(app.width / app.scale + 0.5),
                           (int)(app.height / app.scale + 0.5), app.scale);
    if (app.start_hidden)
        wpe_view_set_visible(wpe_view, FALSE);
    else
        wpe_view_focus_in(wpe_view);

    g_signal_connect(app.web_view, "load-changed", G_CALLBACK(on_load_changed), NULL);
    g_signal_connect(app.web_view, "load-failed", G_CALLBACK(on_load_failed), NULL);
    g_signal_connect(app.web_view, "web-process-terminated", G_CALLBACK(on_web_process_terminated), NULL);
    g_signal_connect(app.web_view, "notify::estimated-load-progress", G_CALLBACK(on_progress), NULL);
    g_signal_connect(app.web_view, "notify::title", G_CALLBACK(on_title), NULL);
    g_signal_connect(app.web_view, "show-option-menu", G_CALLBACK(on_show_option_menu), NULL);
    say("web view %dx%d logical at scale %.2f (%dx%d pixels)%s", wpe_view_get_width(wpe_view),
        wpe_view_get_height(wpe_view), wpe_view_get_scale(wpe_view), app.width, app.height,
        app.start_hidden ? ", hidden" : "");
    return TRUE;
}

static void start(void)
{
    GError *error = NULL;
    if (!create_web_view(&error)) {
        g_printerr("home-companion-web: %s\n", error->message);
        exit(1);
    }
    if (app.socket_path && !body_listen(app.socket_path, &error)) {
        g_printerr("home-companion-web: %s\n", error->message);
        exit(1);
    }
    if (app.token_json) {
        memset(app.token_json, 0, strlen(app.token_json));
        g_clear_pointer(&app.token_json, g_free);
    }
    load(app.url);
    read_next_line();
}

static void on_token_line(GObject *source, GAsyncResult *result, gpointer data)
{
    (void)data;
    char *line = g_data_input_stream_read_line_finish_utf8(G_DATA_INPUT_STREAM(source), result, NULL, NULL);
    if (!line || line[0] != '{') {
        g_printerr("home-companion-web: --token-stdin: the first line of standard input must be the token JSON\n");
        exit(2);
    }
    app.token_json = g_strstrip(line);
    say("token read from standard input");
    start();
}

int main(int argc, char **argv)
{
    app.t0 = g_get_monotonic_time();
    setup_environment();

    GOptionEntry entries[] = {
        {"url", 'u', 0, G_OPTION_ARG_STRING, &app.url, "Page to open (Home Assistant dashboard)", "URL"},
        {"data-dir", 0, 0, G_OPTION_ARG_FILENAME, &app.data_dir, "Persistent data directory (localStorage, IndexedDB)", "DIR"},
        {"cache-dir", 0, 0, G_OPTION_ARG_FILENAME, &app.cache_dir, "Persistent cache directory", "DIR"},
        {"scale", 0, 0, G_OPTION_ARG_DOUBLE, &app.scale, "Device scale factor (default 1.25)", "S"},
        {"width", 0, 0, G_OPTION_ARG_INT, &app.width, "View width in pixels (default 1280)", "PX"},
        {"height", 0, 0, G_OPTION_ARG_INT, &app.height, "View height in pixels (default 800)", "PX"},
        {"render-node", 0, 0, G_OPTION_ARG_FILENAME, &app.render_node, "DRM render node (default /dev/dri/renderD128)", "PATH"},
        {"theme", 0, 0, G_OPTION_ARG_STRING, &app.theme, "Home Assistant theme: dark (default), light or auto", "T"},
        {"token-stdin", 0, 0, G_OPTION_ARG_NONE, &app.token_stdin, "First line of standard input: hassTokens JSON", NULL},
        {"hidden", 0, 0, G_OPTION_ARG_NONE, &app.start_hidden, "Start hidden", NULL},
        {"memory-limit", 0, 0, G_OPTION_ARG_INT, &app.memory_limit_mb, "Web process memory limit (MB)", "MB"},
        {"socket", 0, 0, G_OPTION_ARG_FILENAME, &app.socket_path, "Listen for the body on this Unix socket (SOCK_SEQPACKET)", "PATH"},
        {"png", 0, 0, G_OPTION_ARG_FILENAME, &app.png_path, "Save one frame as PNG once the dashboard is ready", "FILE"},
        {"png-delay", 0, 0, G_OPTION_ARG_DOUBLE, &app.png_delay, "Seconds after ready before --png (default 3)", "S"},
        {NULL, 0, 0, 0, NULL, NULL, NULL},
    };
    GOptionContext *options = g_option_context_new("- Home Assistant in WPE WebKit, off screen, frames as dma-buf");
    g_option_context_add_main_entries(options, entries, NULL);
    GError *error = NULL;
    if (!g_option_context_parse(options, &argc, &argv, &error)) {
        g_printerr("home-companion-web: %s\n", error->message);
        return 2;
    }
    g_option_context_free(options);
    if (!app.url || !app.data_dir || !app.cache_dir) {
        g_printerr("home-companion-web: --url, --data-dir and --cache-dir are required\n");
        return 2;
    }
    if (app.scale < 0.5 || app.scale > 4 || app.width < 64 || app.height < 64) {
        g_printerr("home-companion-web: bad size or scale\n");
        return 2;
    }
    if (!app.theme)
        app.theme = g_strdup("dark");
    if (!app.render_node)
        app.render_node = g_strdup("/dev/dri/renderD128");
    // WPEDRMDevice wants a primary node name; it is never opened (the body is DRM master of the display).
    app.primary_node = g_strdup("/dev/dri/card0");
    g_mkdir_with_parents(app.data_dir, 0700);
    g_mkdir_with_parents(app.cache_dir, 0700);

    app.display = hc_display_new(app.render_node, app.primary_node, &error);
    if (!app.display) {
        g_printerr("home-companion-web: %s\n", error->message);
        return 1;
    }
    wpe_display_set_primary(app.display);
    wpe_display_set_available_input_devices(app.display, WPE_AVAILABLE_INPUT_DEVICE_TOUCHSCREEN);

    app.sent = g_hash_table_new(g_direct_hash, g_direct_equal);
    app.body_held = g_hash_table_new(g_direct_hash, g_direct_equal);
    app.paused = g_array_new(FALSE, FALSE, sizeof(pid_t));
    app.touches = g_hash_table_new_full(g_direct_hash, g_direct_equal, NULL, g_free);
    app.loop = g_main_loop_new(NULL, FALSE);
    g_unix_signal_add(SIGINT, on_signal, NULL);
    g_unix_signal_add(SIGTERM, on_signal, NULL);
    g_timeout_add_seconds(1, fps_cb, NULL);

    GInputStream *stdin_stream = g_unix_input_stream_new(0, FALSE);
    app.input = g_data_input_stream_new(stdin_stream);
    g_object_unref(stdin_stream);
    say("home-companion-web %s, WebKit %u.%u.%u, FD_MESA_DEBUG=%s", app.url, webkit_get_major_version(),
        webkit_get_minor_version(), webkit_get_micro_version(), g_getenv("FD_MESA_DEBUG"));
    if (app.token_stdin)
        g_data_input_stream_read_line_async(app.input, G_PRIORITY_DEFAULT, NULL, on_token_line, NULL);
    else
        start();

    g_main_loop_run(app.loop);
    say("quitting after %u frames", app.frames_total);
    body_disconnect("quitting");
    if (app.listen_fd >= 0) {
        close(app.listen_fd);
        unlink(app.socket_path);
    }
    g_clear_object(&app.web_view);
    g_clear_object(&app.display);
    return 0;
}
