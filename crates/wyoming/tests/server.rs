// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use wyoming::info::tests_support::sample;
use wyoming::{
    AudioFormat, Client, Disconnect, Error, Info, Message, Net, Server, ServerConfig, ServerEvent,
};

const T: Duration = Duration::from_secs(5);

fn start(
    allowed: &[&str],
    ping: Option<Duration>,
    pong_timeout: Duration,
) -> (Server, Receiver<ServerEvent>) {
    let (tx, rx) = mpsc::channel();
    let server = Server::start(
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            allowed: allowed.iter().map(|s| Net::parse(s).unwrap()).collect(),
            info: Info::for_satellite(sample()),
            ping_interval: ping,
            pong_timeout,
            ..Default::default()
        },
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    (server, rx)
}

fn connect(s: &Server) -> Client {
    Client::connect(s.local_addr(), T).unwrap()
}

/// Next event that is not a reported describe/ping.
fn next(rx: &Receiver<ServerEvent>) -> ServerEvent {
    loop {
        let e = rx.recv_timeout(T).expect("server event");
        match &e {
            ServerEvent::Message(Message::Describe | Message::Ping { .. }) => continue,
            _ => return e,
        }
    }
}

fn recv_kind(c: &mut Client, kind: &str) -> Message {
    loop {
        let m = c.recv(Some(T)).unwrap().expect("message");
        if m.kind() == kind {
            return m;
        }
    }
}

#[test]
fn accepted_and_answers_by_itself() {
    let (s, rx) = start(&["127.0.0.1"], None, Duration::from_secs(5));
    let mut c = connect(&s);
    // Before run-satellite: answered, but nothing reaches the sink.
    c.send(&Message::Describe).unwrap();
    let Message::Info(info) = recv_kind(&mut c, "info") else {
        panic!()
    };
    assert_eq!(info, Info::for_satellite(sample()));
    c.send(&Message::Ping { text: None }).unwrap();
    recv_kind(&mut c, "pong");
    c.send(&Message::Played).unwrap(); // ignored: not the satellite link
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    assert!(!s.sender().is_connected());
    // run-satellite: Connected, then the message itself
    c.send(&Message::RunSatellite).unwrap();
    assert!(matches!(
        rx.recv_timeout(T).unwrap(),
        ServerEvent::Connected(_)
    ));
    assert!(matches!(
        rx.recv_timeout(T).unwrap(),
        ServerEvent::Message(Message::RunSatellite)
    ));
    c.send(&Message::Describe).unwrap();
    recv_kind(&mut c, "info");
    c.send(&Message::Ping {
        text: Some("x".into()),
    })
    .unwrap();
    assert_eq!(
        recv_kind(&mut c, "pong"),
        Message::Pong {
            text: Some("x".into())
        }
    );
    // both still reported to the sink
    let kinds: Vec<_> = (0..2)
        .map(|_| match rx.recv_timeout(T).unwrap() {
            ServerEvent::Message(m) => m.kind(),
            e => panic!("{e:?}"),
        })
        .collect();
    assert_eq!(kinds, ["describe", "ping"]);
    // the sender reaches the client
    let sender = s.sender();
    assert!(sender.is_connected());
    assert_eq!(sender.peer().unwrap().ip(), c.peer().ip());
    sender.send(&Message::Played).unwrap();
    assert_eq!(recv_kind(&mut c, "played"), Message::Played);
    sender
        .send_chunk(&AudioFormat::MIC, Some(20), &[1, 2, 3, 4])
        .unwrap();
    sender.send_chunk(&AudioFormat::MIC, None, &[]).unwrap();
    assert_eq!(
        recv_kind(&mut c, "audio-chunk"),
        Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: Some(20),
            audio: vec![1, 2, 3, 4]
        }
    );
    assert_eq!(
        recv_kind(&mut c, "audio-chunk"),
        Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: None,
            audio: vec![]
        }
    );
    // EOF
    drop(c);
    assert!(matches!(
        next(&rx),
        ServerEvent::Disconnected(_, Disconnect::Eof)
    ));
    assert!(matches!(
        sender.send(&Message::Played),
        Err(Error::NotConnected)
    ));
    assert!(!sender.is_connected());
}

#[test]
fn rejected_when_not_allowed() {
    for allowed in [&["192.0.2.0/24"][..], &[]] {
        let (s, rx) = start(allowed, None, Duration::from_secs(5));
        let mut c = connect(&s);
        assert!(matches!(next(&rx), ServerEvent::Rejected(_)));
        // closed at once: the describe gets no answer
        let _ = c.send(&Message::Describe);
        assert!(matches!(c.recv(Some(T)), Ok(None) | Err(_)));
        assert!(!s.sender().is_connected());
    }
}

#[test]
fn learned_addresses_are_let_in_while_it_runs() {
    let learned = std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let s = Server::start(
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            allowed: vec![Net::parse("192.0.2.0/24").unwrap()],
            learned: Some(learned.clone()),
            info: Info::for_satellite(sample()),
            ping_interval: None,
            ..Default::default()
        },
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    let _c = connect(&s);
    assert!(matches!(next(&rx), ServerEvent::Rejected(_)));
    learned
        .write()
        .unwrap()
        .push(Net::parse("127.0.0.1").unwrap());
    let (_c, _) = link(&s, &rx);
    assert!(s.sender().is_connected());
}

/// Connects and sends run-satellite; returns the link's address as the server saw it.
fn link(s: &Server, rx: &Receiver<ServerEvent>) -> (Client, SocketAddr) {
    let mut c = connect(s);
    c.send(&Message::RunSatellite).unwrap();
    let ServerEvent::Connected(p) = next(rx) else {
        panic!()
    };
    assert!(matches!(
        next(rx),
        ServerEvent::Message(Message::RunSatellite)
    ));
    (c, p)
}

#[test]
fn second_run_satellite_replaces_the_link() {
    let (s, rx) = start(&["127.0.0.0/8"], None, Duration::from_secs(5));
    let (mut a, pa) = link(&s, &rx);
    let mut b = connect(&s);
    // a plain connection does not replace anything
    b.send(&Message::Describe).unwrap();
    recv_kind(&mut b, "info");
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    b.send(&Message::RunSatellite).unwrap();
    // the old reader reports first, then the new link
    match next(&rx) {
        ServerEvent::Disconnected(p, Disconnect::Replaced) => assert_eq!(p, pa),
        e => panic!("{e:?}"),
    }
    let ServerEvent::Connected(pb) = next(&rx) else {
        panic!()
    };
    assert_ne!(pa, pb);
    assert!(matches!(
        next(&rx),
        ServerEvent::Message(Message::RunSatellite)
    ));
    assert!(matches!(a.recv(Some(T)), Ok(None) | Err(_)));
    b.send(&Message::Describe).unwrap();
    recv_kind(&mut b, "info");
    assert_eq!(s.sender().peer().unwrap().port(), pb.port());
}

#[test]
fn probes_do_not_disturb_the_link() {
    // Home Assistant's satellite connection with its pings, plus short describe-only
    // connections (load_wyoming_info) while it is up: the link stays, nothing is reported.
    let (s, rx) = start(
        &["127.0.0.1"],
        Some(Duration::from_millis(200)),
        Duration::from_secs(5),
    );
    let (mut c, pc) = link(&s, &rx);
    for _ in 0..5 {
        let mut p = connect(&s);
        p.send(&Message::Describe).unwrap();
        recv_kind(&mut p, "info");
        // a probe left open while another one comes
        let mut q = connect(&s);
        q.send(&Message::Describe).unwrap();
        recv_kind(&mut q, "info");
        drop(p);
        drop(q);
        // the link keeps being pinged and answers
        let Message::Ping { text } = recv_kind(&mut c, "ping") else {
            panic!()
        };
        c.send(&Message::Pong { text }).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(e) = rx.try_recv() {
        assert!(
            matches!(e, ServerEvent::Message(_)),
            "probe disturbed the link: {e:?}"
        );
    }
    assert_eq!(s.sender().peer(), Some(pc));
    s.sender().send(&Message::Played).unwrap();
    recv_kind(&mut c, "played");
}

#[test]
fn no_pings_while_paused() {
    let (s, rx) = start(
        &["127.0.0.1"],
        Some(Duration::from_millis(100)),
        Duration::from_millis(400),
    );
    let (mut c, _) = link(&s, &rx);
    recv_kind(&mut c, "ping"); // not answered: paused right after
    c.send(&Message::PauseSatellite).unwrap();
    assert!(matches!(
        next(&rx),
        ServerEvent::Message(Message::PauseSatellite)
    ));
    // a muted Home Assistant does not read: no ping, no pong timeout
    match c.recv(Some(Duration::from_millis(1200))) {
        Err(e) if wyoming::client::is_timeout(&e) => {}
        r => panic!("{r:?}"),
    }
    assert!(s.sender().is_connected());
    // run-satellite again: pings resume
    c.send(&Message::RunSatellite).unwrap();
    recv_kind(&mut c, "ping");
}

#[test]
fn pong_timeout() {
    let (s, rx) = start(
        &["127.0.0.1"],
        Some(Duration::from_millis(200)),
        Duration::from_millis(500),
    );
    let (mut c, _) = link(&s, &rx);
    // a client that answers: still connected after several rounds
    let t0 = Instant::now();
    let mut pings = 0;
    while t0.elapsed() < Duration::from_millis(1500) {
        match c.recv(Some(Duration::from_millis(100))) {
            Ok(Some(Message::Ping { text })) => {
                pings += 1;
                c.send(&Message::Pong { text }).unwrap();
            }
            Ok(Some(m)) => panic!("{m:?}"),
            Ok(None) => panic!("closed"),
            Err(e) if wyoming::client::is_timeout(&e) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert!(pings >= 3, "{pings} pings");
    assert!(rx.try_recv().is_err(), "pongs to our pings are swallowed");
    // now stop answering
    let t1 = Instant::now();
    match next(&rx) {
        ServerEvent::Disconnected(_, Disconnect::PongTimeout) => {}
        e => panic!("{e:?}"),
    }
    let took = t1.elapsed();
    assert!(took < Duration::from_millis(1500), "{took:?}");
    drop(c);
}

#[test]
fn many_probes() {
    let (s, rx) = start(
        &["127.0.0.1"],
        Some(Duration::from_secs(2)),
        Duration::from_secs(5),
    );
    for _ in 0..100 {
        let mut c = connect(&s);
        c.send(&Message::Describe).unwrap();
        recv_kind(&mut c, "info");
    }
    // probes are invisible to the sink
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    // still serving
    let mut c = connect(&s);
    c.send(&Message::Describe).unwrap();
    recv_kind(&mut c, "info");
}

#[test]
fn not_connected_and_shutdown() {
    let (s, rx) = start(
        &["127.0.0.1"],
        Some(Duration::from_secs(2)),
        Duration::from_secs(5),
    );
    assert!(matches!(
        s.sender().send(&Message::Played),
        Err(Error::NotConnected)
    ));
    assert!(matches!(
        s.sender().send_chunk(&AudioFormat::MIC, None, &[0; 4]),
        Err(Error::NotConnected)
    ));
    let (mut c, _) = link(&s, &rx);
    let mut probe = connect(&s);
    let addr: SocketAddr = s.local_addr();
    let sender = s.sender();
    let t0 = Instant::now();
    s.shutdown();
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert!(matches!(
        next(&rx),
        ServerEvent::Disconnected(_, Disconnect::Shutdown)
    ));
    assert!(matches!(c.recv(Some(T)), Ok(None) | Err(_)));
    assert!(matches!(probe.recv(Some(T)), Ok(None) | Err(_)));
    assert!(!sender.is_connected());
    // the port is free again
    assert!(Client::connect(addr, Duration::from_millis(500)).is_err());
}

#[test]
fn bad_frame_closes_the_connection() {
    use std::io::Write;
    let (s, rx) = start(&["127.0.0.1"], None, Duration::from_secs(5));
    let (c, _) = link(&s, &rx);
    let mut raw = c.try_clone_stream().unwrap();
    raw.write_all(b"{\"type\": \"x\", \"payload_length\": 99999999999}\n")
        .unwrap();
    match next(&rx) {
        ServerEvent::Disconnected(_, Disconnect::Io(why)) => {
            assert!(why.contains("bad frame"), "{why}")
        }
        e => panic!("{e:?}"),
    }
}
