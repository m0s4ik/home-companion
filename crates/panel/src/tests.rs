// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

use super::*;
use std::io::Read;
use std::sync::Arc;

const GOOD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ALLOWED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TICKET: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ID: &str = "dddddddddddddddddddddddddddddddd";

/// A body that answers like the real one, and remembers what it was asked.
#[derive(Default)]
struct Fake {
    asked: Mutex<Vec<Value>>,
}

impl Body for Fake {
    fn call(&self, r: Value) -> Option<Value> {
        self.asked.lock().unwrap().push(r.clone());
        let session = r["token"] == GOOD;
        Some(match r["op"].as_str().unwrap_or("") {
            "hello" => json!({"ok": true, "name": "Alfred", "on": true, "session": session}),
            "strings" => json!({"ok": true, "lang": "en", "strings": {}}),
            "pair.request" => json!({"ok": true, "id": ID, "code": "4827"}),
            "pair.status" => json!({"ok": true, "status": "allowed", "token": ALLOWED}),
            "pair.qr" if r["ticket"] == TICKET => json!({"ok": true, "token": ALLOWED}),
            "pair.qr" => json!({"ok": false, "error": "bad_ticket"}),
            _ if !session => json!({"ok": false, "error": "no_session"}),
            "set" if r["key"] == "nope" => json!({"ok": false, "error": "unknown_key"}),
            _ => json!({"ok": true, "values": {}}),
        })
    }
}

fn panel() -> Panel<Fake> {
    Panel {
        body: Fake::default(),
        hostname: "thinksmart".into(),
        rate: RateLimit::default(),
    }
}

const PEER: &str = "192.0.2.24";

fn lan() -> IpAddr {
    // 192.0.2.x is documentation space, not private: the tests' browser is on 10.x.
    "10.0.0.24".parse().unwrap()
}

fn req(method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> Request {
    let mut h: Vec<(String, String)> = vec![("Host".into(), "10.0.0.5:8125".into())];
    h.extend(headers.iter().map(|(a, b)| (a.to_string(), b.to_string())));
    Request {
        method: method.into(),
        target: target.into(),
        headers: h,
        body: body.as_bytes().to_vec(),
    }
}

fn post(target: &str, body: &str, extra: &[(&str, &str)]) -> Request {
    let mut h = vec![
        ("Origin", "http://10.0.0.5:8125"),
        ("Content-Type", "application/json"),
        (
            "Cookie",
            "theme=dark; hc_panel=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ];
    h.extend_from_slice(extra);
    req("POST", target, &h, body)
}

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn status(p: &Panel<Fake>, r: &Request) -> u16 {
    p.route(r, lan(), Instant::now()).status
}

#[test]
fn pages_and_their_headers() {
    let p = panel();
    for (path, ct) in [
        ("/", "text/html; charset=utf-8"),
        ("/panel.css", "text/css; charset=utf-8"),
        ("/panel.js", "text/javascript; charset=utf-8"),
    ] {
        let r = p.route(&req("GET", path, &[], ""), lan(), Instant::now());
        assert_eq!(r.status, 200);
        assert_eq!(header(&r, "content-type"), Some(ct));
        assert!(
            header(&r, "content-security-policy")
                .unwrap()
                .contains("script-src 'self'")
        );
        assert_eq!(header(&r, "x-frame-options"), Some("DENY"));
        assert_eq!(header(&r, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&r, "referrer-policy"), Some("no-referrer"));
        assert!(header(&r, "access-control-allow-origin").is_none());
        assert_eq!(header(&r, "cache-control"), Some("no-store"));
    }
    // The previews' letters: the screen's Inter, kept a day, and nothing else of the disk.
    for (path, len) in [
        ("/inter-400.ttf", FONT_REGULAR.len()),
        ("/inter-500.ttf", FONT_MEDIUM.len()),
    ] {
        let r = p.route(&req("GET", path, &[], ""), lan(), Instant::now());
        assert_eq!((r.status, r.body.len()), (200, len));
        assert_eq!(header(&r, "content-type"), Some("font/ttf"));
        assert_eq!(header(&r, "cache-control"), Some("public, max-age=86400"));
        assert!(
            header(&r, "content-security-policy")
                .unwrap()
                .contains("font-src 'self'")
        );
    }
    assert!(FONT_REGULAR.starts_with(&[0, 1, 0, 0]) && FONT_MEDIUM.starts_with(&[0, 1, 0, 0]));
    assert_eq!(status(&p, &req("GET", "/inter-700.ttf", &[], "")), 404);
    // An answer of the body is never kept.
    let r = p.route(&req("GET", "/api/hello", &[], ""), lan(), Instant::now());
    assert_eq!(header(&r, "cache-control"), Some("no-store"));
    assert_eq!(status(&p, &req("GET", "/etc/passwd", &[], "")), 404);
    assert_eq!(status(&p, &req("PUT", "/api/set", &[], "")), 405);
}

#[test]
fn only_the_home_network_and_its_names() {
    let p = panel();
    let r = req("GET", "/", &[], "");
    assert_eq!(
        p.route(&r, PEER.parse().unwrap(), Instant::now()).status,
        403
    );
    assert_eq!(
        p.route(&r, "8.8.8.8".parse().unwrap(), Instant::now())
            .status,
        403
    );
    // DNS rebinding: another site's name pointing here.
    for host in [
        "evil.example",
        "evil.example:8125",
        "8.8.8.8:8125",
        "thinksmart.evil",
    ] {
        let mut r = req("GET", "/api/hello", &[], "");
        r.headers[0].1 = host.into();
        assert_eq!(status(&p, &r), 421, "{host}");
    }
    for host in [
        "10.0.0.5",
        "[fd00::5]:8125",
        "thinksmart.local:8125",
        "ThinkSmart",
        "localhost:8125",
    ] {
        let mut r = req("GET", "/api/hello", &[], "");
        r.headers[0].1 = host.into();
        assert_eq!(status(&p, &r), 200, "{host}");
    }
    let mut r = req("GET", "/", &[], "");
    r.headers.clear();
    assert_eq!(status(&p, &r), 421);
}

#[test]
fn writes_need_the_same_origin_and_json() {
    let p = panel();
    let ok = post("/api/set", r#"{"key":"home_compact","value":true}"#, &[]);
    assert_eq!(status(&p, &ok), 200);
    // No Origin, another Origin, another site by the browser's own word.
    let mut r = ok.clone();
    r.headers.retain(|(n, _)| n != "Origin");
    assert_eq!(status(&p, &r), 403);
    let mut r = ok.clone();
    r.headers[1].1 = "http://evil.example".into();
    assert_eq!(status(&p, &r), 403);
    let mut r = ok.clone();
    r.headers[1].1 = "http://10.0.0.5:8125.evil.example".into();
    assert_eq!(status(&p, &r), 403);
    let r = post("/api/set", "{}", &[("Sec-Fetch-Site", "cross-site")]);
    assert_eq!(status(&p, &r), 403);
    // A "simple" request (a form): refused.
    let mut r = ok.clone();
    r.headers[2].1 = "text/plain".into();
    assert_eq!(status(&p, &r), 415);
    let mut r = ok.clone();
    r.headers[2].1 = "application/x-www-form-urlencoded".into();
    assert_eq!(status(&p, &r), 415);
    assert_eq!(status(&p, &post("/api/set", "not json", &[])), 400);
    assert_eq!(status(&p, &post("/api/set", "[1]", &[])), 400);
}

#[test]
fn without_the_cookie_401() {
    let p = panel();
    assert_eq!(status(&p, &req("GET", "/api/state", &[], "")), 401);
    let bad = req("GET", "/api/state", &[("Cookie", "hc_panel=1234")], "");
    assert_eq!(status(&p, &bad), 401);
    let good = req(
        "GET",
        "/api/state",
        &[("Cookie", &format!("hc_panel={GOOD}"))],
        "",
    );
    assert_eq!(status(&p, &good), 200);
    let mut r = post("/api/set", "{}", &[]);
    r.headers.retain(|(n, _)| n != "Cookie");
    assert_eq!(status(&p, &r), 401);
}

/// The token goes into an HttpOnly cookie and never into a body the page's script can read.
#[test]
fn the_token_only_in_the_cookie() {
    let p = panel();
    let r = p.route(
        &req("GET", &format!("/api/pair/{ID}"), &[], ""),
        lan(),
        Instant::now(),
    );
    assert_eq!(r.status, 200);
    let cookie = header(&r, "set-cookie").unwrap();
    assert!(cookie.starts_with(&format!("hc_panel={ALLOWED};")));
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/")
    );
    assert!(!String::from_utf8_lossy(&r.body).contains(ALLOWED));
    assert_eq!(status(&p, &req("GET", "/api/pair/xyz", &[], "")), 400);
    // The QR code: in, then the page.
    let r = p.route(
        &req("GET", &format!("/q/{TICKET}"), &[], ""),
        lan(),
        Instant::now(),
    );
    assert_eq!(r.status, 303);
    assert_eq!(header(&r, "location"), Some("/"));
    assert!(
        header(&r, "set-cookie")
            .unwrap()
            .starts_with(&format!("hc_panel={ALLOWED};"))
    );
    let r = p.route(
        &req("GET", &format!("/q/{}", "e".repeat(64)), &[], ""),
        lan(),
        Instant::now(),
    );
    assert_eq!(header(&r, "location"), Some("/?qr=bad_ticket"));
    assert!(header(&r, "set-cookie").is_none());
    // A HEAD (a link preview's probe) does not spend it.
    let before = p.body.asked.lock().unwrap().len();
    let r = p.route(
        &req("HEAD", &format!("/q/{TICKET}"), &[], ""),
        lan(),
        Instant::now(),
    );
    assert!(header(&r, "set-cookie").is_none());
    assert_eq!(p.body.asked.lock().unwrap().len(), before);
    let r = p.route(
        &req("GET", &format!("/q/{}", "c".repeat(32)), &[], ""),
        lan(),
        Instant::now(),
    );
    assert_eq!(header(&r, "location"), Some("/?qr=bad_ticket"));
    // Log out: the cookie goes.
    let r = p.route(&post("/api/logout", "{}", &[]), lan(), Instant::now());
    assert!(header(&r, "set-cookie").unwrap().contains("Max-Age=0"));
}

/// The body gets the session, the address and the browser as this process saw them, and only
/// the fields of each request: whatever else the page sends stays here.
#[test]
fn the_body_gets_only_what_it_should() {
    let p = panel();
    let r = post(
        "/api/set",
        r#"{"key":"home_compact","value":true,"token":"x","peer":"8.8.8.8","op":"sessions.revoke"}"#,
        &[
            (
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 Chrome/131.0 Safari/537.36",
            ),
            ("Accept-Language", "it-IT,it;q=0.9,en;q=0.8"),
        ],
    );
    assert_eq!(status(&p, &r), 200);
    let asked = p.body.asked.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        asked,
        json!({"op": "set", "token": GOOD, "peer": "10.0.0.24", "agent": {"browser": "Chrome", "os": "macOS"},
               "lang": ["it-IT", "it", "en"], "key": "home_compact", "value": true})
    );
    assert_eq!(status(&p, &post("/api/set", r#"{"key":"nope"}"#, &[])), 400);
}

#[test]
fn twenty_a_second() {
    let p = panel();
    let t = Instant::now();
    let r = req("GET", "/api/hello", &[], "");
    for _ in 0..PER_SECOND {
        assert_eq!(p.route(&r, lan(), t).status, 200);
    }
    let r429 = p.route(&r, lan(), t);
    assert_eq!(r429.status, 429);
    assert_eq!(header(&r429, "retry-after"), Some("1"));
    // Another address is not slowed; the same one is fine a second later.
    assert_eq!(p.route(&r, "10.0.0.25".parse().unwrap(), t).status, 200);
    assert_eq!(p.route(&r, lan(), t + Duration::from_secs(1)).status, 200);
}

#[test]
fn browsers_and_languages() {
    assert_eq!(
        agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 Version/18.1 Mobile/15E148 Safari/604.1"
        ),
        ("Safari", "iPhone")
    );
    assert_eq!(
        agent("Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0"),
        ("Firefox", "Linux")
    );
    assert_eq!(
        agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36 Edg/131.0"
        ),
        ("Edge", "Windows")
    );
    assert_eq!(
        agent(
            "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 Chrome/131.0 Mobile Safari/537.36"
        ),
        ("Chrome", "Android")
    );
    assert_eq!(agent(""), ("Browser", "?"));
    assert_eq!(
        languages("en;q=0.5, it-IT , de;q=0.8"),
        ["it-IT", "de", "en"]
    );
    assert!(languages("").is_empty());
}

/// Over real sockets: a fake body on a Unix socket, the panel on TCP loopback; a body over the
/// limit is 413; nobody asking for `idle`: the process ends.
#[test]
fn over_sockets_and_idle() {
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("hc-panel-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("panel.sock");
    let ul = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for c in ul.incoming() {
            let Ok(c) = c else { continue };
            let mut w = c.try_clone().unwrap();
            let mut line = String::new();
            if BufReader::new(c).read_line(&mut line).is_ok() {
                let r: Value = serde_json::from_str(&line).unwrap();
                let a = Fake::default().call(r).unwrap();
                let _ = writeln!(w, "{a}");
            }
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let p = Arc::new(Panel {
        body: SocketBody { path: sock },
        hostname: String::new(),
        rate: RateLimit::default(),
    });
    let server = std::thread::spawn(move || run(p, listener, Duration::from_millis(1500)));
    let raw = |r: String| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _ = s.write_all(r.as_bytes());
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let hello = raw(format!(
        "GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    ));
    assert!(hello.starts_with("HTTP/1.1 200"), "{hello}");
    assert!(hello.ends_with(r#"{"name":"Alfred","ok":true,"on":true,"session":false}"#));
    let big = format!(
        "POST /api/set HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n",
        (64 << 10) + 1
    );
    assert!(raw(big).starts_with("HTTP/1.1 413"));
    // As many slow connections from one address as allowed: one more is closed at once.
    let slow: Vec<TcpStream> = (0..PER_ADDRESS)
        .map(|_| {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET / HT").unwrap();
            s
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let mut fourth = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let _ = fourth.write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    let mut out = String::new();
    let _ = fourth.read_to_string(&mut out);
    assert!(out.is_empty(), "{out}");
    drop(slow);
    std::thread::sleep(Duration::from_millis(300));
    let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(17 << 10));
    assert!(raw(long).starts_with("HTTP/1.1 431"));
    // Idle: it ends by itself.
    let t = Instant::now();
    server.join().unwrap().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pages are small (they are in the binary) and draw text only as text.
#[test]
fn pages_are_light() {
    // About 60 KB since the widget's previews are drawn here (panel v2); the fonts apart.
    let total = INDEX.len() + CSS.len() + JS.len();
    assert!(total < 72 * 1024, "{total} bytes");
    // No style or script in the page itself, none set from the script as an attribute.
    assert!(!JS.contains("setAttribute('style'") && !JS.contains(".style."));
    assert!(
        !JS.contains("innerHTML")
            && !JS.contains("outerHTML")
            && !JS.contains("insertAdjacentHTML")
    );
    assert!(!JS.contains("eval(") && !JS.contains("document.write"));
    assert!(!INDEX.contains("<script>") && !INDEX.contains("style="));
}

#[test]
fn the_widgets_routes() {
    let p = panel();
    let last = |p: &Panel<Fake>| p.body.asked.lock().unwrap().last().cloned().unwrap();
    // Reads: the widgets, a search by its id (32 hex), nothing else in the path.
    assert_eq!(
        status(
            &p,
            &req(
                "GET",
                "/api/widgets",
                &[("Cookie", &format!("hc_panel={GOOD}"))],
                ""
            )
        ),
        200
    );
    assert_eq!(last(&p)["op"], "widgets.get");
    let get = |path: &str| req("GET", path, &[("Cookie", &format!("hc_panel={GOOD}"))], "");
    assert_eq!(status(&p, &get(&format!("/api/search/{ID}"))), 200);
    assert_eq!(last(&p)["op"], "search.status");
    assert_eq!(last(&p)["id"], ID);
    for bad in [
        "/api/search/x",
        "/api/search/../etc",
        &format!("/api/search/{ID}0"),
    ] {
        assert_eq!(status(&p, &get(bad)), 400, "{bad}");
    }
    // Writes: only their fields reach the body.
    let r = post(
        "/api/search",
        r#"{"what":"quote","q":"Apple","op":"set","name":"x"}"#,
        &[],
    );
    assert_eq!(status(&p, &r), 200);
    let a = last(&p);
    assert_eq!(
        (a["op"].as_str(), a["what"].as_str(), a["q"].as_str()),
        (Some("search.start"), Some("quote"), Some("Apple"))
    );
    assert!(a.get("name").is_none());
    for (path, op) in [
        ("/api/quotes/add", "quotes.add"),
        ("/api/quotes/remove", "quotes.remove"),
        ("/api/quotes/move", "quotes.move"),
        ("/api/cities/add", "cities.add"),
        ("/api/cities/remove", "cities.remove"),
        ("/api/cities/move", "cities.move"),
        ("/api/widgets/order", "widgets.order"),
        ("/api/sensors/add", "sensors.add"),
        ("/api/sensors/move", "sensors.move"),
        ("/api/player", "player.set"),
        ("/api/people", "people.set"),
    ] {
        let r = post(
            path,
            r#"{"search":"s","ref":"yahoo:AAPL","to":1,"order":["next"],"lat":1}"#,
            &[],
        );
        assert_eq!(status(&p, &r), 200, "{path}");
        let a = last(&p);
        assert_eq!(a["op"], op);
        assert!(a.get("lat").is_none());
    }
    assert_eq!(status(&p, &post("/api/quotes/delete", "{}", &[])), 404);
    let p = panel();
    for (path, op) in [
        ("/api/notes/add", "notes.add"),
        ("/api/notes/edit", "notes.edit"),
        ("/api/notes/remove", "notes.remove"),
    ] {
        assert_eq!(
            status(&p, &post(path, r#"{"id":1,"text":"x","list":"y"}"#, &[])),
            200
        );
        let a = last(&p);
        assert_eq!(
            (a["op"].as_str(), a["text"].as_str()),
            (Some(op), Some("x"))
        );
        assert!(a.get("list").is_none());
    }
    assert_eq!(status(&p, &get("/api/notes")), 200);
    assert_eq!(last(&p)["op"], "notes.list");
    assert_eq!(status(&p, &get("/api/widgets/samples")), 200);
    assert_eq!(last(&p)["op"], "widgets.samples");
    assert_eq!(
        status(&p, &post("/api/undo", r#"{"ref":"x","op":"set"}"#, &[])),
        200
    );
    let a = last(&p);
    assert_eq!(a["op"], "undo");
    assert!(a.get("ref").is_none());
    assert_eq!(status(&p, &get("/api/undo")), 404);
    assert_eq!(status(&p, &post("/api/notes/clear", "{}", &[])), 404);
    // Without the same origin: refused before the body hears of it.
    let n = p.body.asked.lock().unwrap().len();
    let r = post("/api/quotes/add", "{}", &[("Sec-Fetch-Site", "cross-site")]);
    assert_eq!(status(&p, &r), 403);
    assert_eq!(p.body.asked.lock().unwrap().len(), n);
}
