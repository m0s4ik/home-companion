// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The connection to the helper, on a thread of its own so that nothing the helper does (or
//! fails to do) can block the UI: connect with a timeout (a stopped helper does not accept),
//! send the body's `hello`, wait for the helper's (a hung one never answers), then read
//! messages and hand them to the UI thread through a queue and `invoke_from_event_loop`.
//! The UI thread writes on the same socket, non-blocking (link `send` in mod.rs).

use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use webview_proto::{self as proto, Hello, Message, Recv};

pub enum Event {
    /// A helper answered our hello. `pid`: the helper's (SO_PEERCRED), for its cgroup.
    Connected {
        sock: Arc<OwnedFd>,
        hello: Hello,
        pid: Option<i32>,
    },
    Message(Message, Vec<OwnedFd>),
    /// The connection is over (EOF, error); the thread reconnects.
    Disconnected(String),
}

/// Events for the UI thread, tagged with the connection they belong to.
pub type Queue = Arc<Mutex<VecDeque<(u64, Event)>>>;

pub struct Link {
    stop: Arc<AtomicBool>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn poll_in(fd: &OwnedFd, ms: i32) -> std::io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let r = unsafe { libc::poll(&mut p, 1, ms) };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e);
    }
    Ok(r > 0)
}

/// Starts the thread. `notify` runs on it after events were queued (it wakes the UI thread).
pub fn spawn(
    path: PathBuf,
    hello: Hello,
    queue: Queue,
    notify: impl Fn() + Send + 'static,
) -> std::io::Result<Link> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    std::thread::Builder::new()
        .name("web-link".into())
        .spawn(move || run(path, hello, queue, notify, s))?;
    Ok(Link { stop })
}

fn run(path: PathBuf, hello: Hello, queue: Queue, notify: impl Fn(), stop: Arc<AtomicBool>) {
    let mut generation = 0u64;
    let mut backoff = Duration::from_millis(500);
    let mut last_error = String::new();
    let push = |g: u64, e: Event| {
        queue.lock().unwrap().push_back((g, e));
        notify();
    };
    while !stop.load(Ordering::Relaxed) {
        let sock = match proto::connect_timeout(&path, Duration::from_secs(1)) {
            Ok(s) => s,
            Err(e) => {
                let why = format!("{}: {e}", path.display());
                if why != last_error {
                    log::info!("web view: no helper ({why}): retrying");
                    last_error = why;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        let pid = proto::peer_pid(sock.as_fd()).ok().filter(|&p| p > 0);
        // A helper left frozen by a body that died never answers: thaw it first.
        if let Some(f) = pid.and_then(|p| super::system::freeze_file(p).ok()) {
            drop(super::system::Freezer::new(f));
        }
        if let Err(e) = proto::send(sock.as_fd(), &Message::Hello(hello), &[]) {
            log::warn!("web view: hello: {e}");
            std::thread::sleep(backoff);
            continue;
        }
        // The helper's hello (it sends hello, state and its latest frame at once).
        let theirs = match poll_in(&sock, 5000) {
            Ok(true) => match proto::recv(sock.as_fd()) {
                Ok(Recv::Message(Message::Hello(h), _)) => Some(h),
                Ok(other) => {
                    log::warn!("web view: expected hello, got {other:?}");
                    None
                }
                Err(e) => {
                    log::warn!("web view: hello: {e}");
                    None
                }
            },
            _ => {
                log::warn!("web view: the helper accepted but did not answer in 5 s (hung?)");
                None
            }
        };
        let Some(theirs) = theirs else {
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(5));
            continue;
        };
        if let Err(e) = proto::set_nonblocking(sock.as_fd(), true) {
            log::warn!("web view: non-blocking socket: {e}");
            continue;
        }
        backoff = Duration::from_millis(500);
        last_error.clear();
        generation += 1;
        let g = generation;
        let sock = Arc::new(sock);
        log::info!(
            "web view: helper connected (view {}x{}, scale {:.2}, pid {})",
            theirs.width,
            theirs.height,
            theirs.scale_milli as f64 / 1000.0,
            pid.map_or("?".into(), |p| p.to_string())
        );
        push(
            g,
            Event::Connected {
                sock: sock.clone(),
                hello: theirs,
                pid,
            },
        );
        let why = loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match poll_in(&sock, 500) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => break format!("poll: {e}"),
            }
            let mut batch = Vec::new();
            let end = loop {
                match proto::recv(sock.as_fd()) {
                    Ok(Recv::Message(m, fds)) => batch.push(Event::Message(m, fds)),
                    Ok(Recv::Eof) => break Some("the helper closed the connection".to_string()),
                    Err(proto::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        break None;
                    }
                    Err(e) if e.is_unknown() => log::debug!("web view: {e} (skipped)"),
                    Err(e) => break Some(format!("receive: {e}")),
                }
            };
            if !batch.is_empty() {
                let mut q = queue.lock().unwrap();
                q.extend(batch.into_iter().map(|e| (g, e)));
                drop(q);
                notify();
            }
            if let Some(why) = end {
                break why;
            }
        };
        push(g, Event::Disconnected(why));
        std::thread::sleep(Duration::from_millis(300));
    }
}
