// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's end of `panel.sock`: the browser panel's process (`home-companion-panel`) connects
//! here, and only here, to do what [`super::protocol`] lists. One JSON object per line, one answer
//! per line. The socket is 0660, group `home-companion-panel` (the body's user is in that group,
//! the panel's user is its only other member); a connection from any other user is closed at once
//! (SO_PEERCRED).
//!
//! The answers are made on the screen's thread (the sessions and the settings live there): each
//! request goes to it as a [`Call`] with a channel for the answer, picked up by its poll.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{Value, json};

/// The socket's default place.
/// In a directory of its own: the panel's unit sees only that one of `/run/home-companion`.
pub const DEFAULT_PATH: &str = "/run/home-companion/panel/panel.sock";
/// The group that may connect (packaging/install.sh makes it).
pub const GROUP: &str = "home-companion-panel";
/// A request line is at most this long (the panel's own body limit is 64 KiB).
pub const MAX_LINE: usize = 80 << 10;
const MAX_CONNECTIONS: usize = 8;
/// How long the screen's thread may take to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// A request for the screen's thread and where its answer goes.
pub struct Call {
    pub request: Value,
    pub reply: Sender<Value>,
}

/// The listener: requests come out of `calls`; the socket file goes when this is dropped.
pub struct PanelSocket {
    pub calls: Receiver<Call>,
    path: PathBuf,
}

impl Drop for PanelSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lookup_gid(name: &str) -> Option<libc::gid_t> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: getgrnam returns a pointer into static storage or null; read at once.
    let g = unsafe { libc::getgrnam(c.as_ptr()) };
    (!g.is_null()).then(|| unsafe { (*g).gr_gid })
}

fn lookup_uid(name: &str) -> Option<libc::uid_t> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: as above.
    let p = unsafe { libc::getpwnam(c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { (*p).pw_uid })
}

/// The uid at the other end of a Unix stream.
fn peer_uid(s: &UnixStream) -> Option<libc::uid_t> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the size given.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

/// Listens on `path` (an old socket file there is replaced).
pub fn start(path: &Path) -> std::io::Result<PanelSocket> {
    use std::os::unix::fs::PermissionsExt;
    let gid = lookup_gid(GROUP);
    let chgrp = |p: &Path| {
        let Some(gid) = gid else { return };
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
            return;
        };
        // SAFETY: a valid path; uid -1 keeps the owner.
        if unsafe { libc::chown(c.as_ptr(), u32::MAX, gid) } != 0 {
            log::warn!(event = "panel"; "{}: chgrp {GROUP}: {}", p.display(), std::io::Error::last_os_error());
        }
    };
    // The directory: 0750, group home-companion-panel (the panel's unit binds only it).
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o750))?;
        chgrp(d);
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    chgrp(path);
    // SAFETY: getuid never fails.
    let me = unsafe { libc::getuid() };
    let allowed: Vec<libc::uid_t> = std::iter::once(me).chain(lookup_uid(GROUP)).collect();
    let (tx, rx) = mpsc::channel();
    let open = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("panel-socket".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                if !peer_uid(&stream).is_some_and(|u| allowed.contains(&u)) {
                    log::warn!(event = "panel"; "panel socket: a connection from another user, closed");
                    continue;
                }
                if open.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    continue;
                }
                open.fetch_add(1, Ordering::Relaxed);
                let (tx, open) = (tx.clone(), open.clone());
                let _ = std::thread::Builder::new()
                    .name("panel-conn".into())
                    .spawn(move || {
                        serve(stream, &tx);
                        open.fetch_sub(1, Ordering::Relaxed);
                    });
            }
        })?;
    log::info!(event = "panel"; "browser panel socket at {}", path.display());
    Ok(PanelSocket {
        calls: rx,
        path: path.to_path_buf(),
    })
}

fn serve(stream: UnixStream, tx: &Sender<Call>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(mut w) = stream.try_clone() else {
        return;
    };
    let mut r = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match (&mut r).take(MAX_LINE as u64).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with('\n') => return,
            Ok(_) => {}
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(v) if v.is_object() => {
                let (reply, back) = mpsc::channel();
                if tx.send(Call { request: v, reply }).is_err() {
                    return;
                }
                back.recv_timeout(ANSWER_WITHIN)
                    .unwrap_or_else(|_| json!({"ok": false, "error": "busy"}))
            }
            _ => json!({"ok": false, "error": "bad_request"}),
        };
        if writeln!(w, "{answer}").and_then(|()| w.flush()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_in_answers_out() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-panel-sock-{}", std::process::id()));
        let path = dir.join("panel.sock");
        let sock = start(&path).unwrap();
        let dmode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&dir).unwrap().permissions(),
        );
        assert_eq!(dmode & 0o777, 0o750);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o660);
        let client = {
            let path = path.clone();
            std::thread::spawn(move || {
                let s = UnixStream::connect(&path).unwrap();
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                let mut ask = |line: &str| {
                    w.write_all(line.as_bytes()).unwrap();
                    let mut out = String::new();
                    r.read_line(&mut out).unwrap();
                    serde_json::from_str::<Value>(&out).unwrap()
                };
                assert_eq!(
                    ask("{\"op\":\"hello\"}\n"),
                    json!({"ok": true, "op": "hello"})
                );
                assert_eq!(ask("[1]\n")["error"], "bad_request");
                assert_eq!(ask("nope\n")["error"], "bad_request");
                assert_eq!(ask("{\"op\":\"state\"}\n")["op"], "state");
            })
        };
        // The screen's thread: echoes the op.
        while !client.is_finished() {
            if let Ok(c) = sock.calls.recv_timeout(Duration::from_millis(20)) {
                let _ = c.reply.send(json!({"ok": true, "op": c.request["op"]}));
            }
        }
        client.join().unwrap();
        // A line over the limit closes the connection.
        let mut s = UnixStream::connect(&path).unwrap();
        let _ = s.write_all(&vec![b'a'; MAX_LINE + 10]);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
