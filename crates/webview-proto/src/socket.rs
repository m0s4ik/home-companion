// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `SOCK_SEQPACKET` Unix sockets and `SCM_RIGHTS`, with libc only.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::{Error, MAX_MESSAGE, MAX_PLANES, Message};

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn seqpacket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the fd is owned from here on.
    let fd =
        cvt(unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) })?;
    // SAFETY: fd was just returned by socket(2).
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: sockaddr_un is plain data; all zeros is valid.
    let mut a: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    a.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let p = path.as_os_str().as_bytes();
    if p.len() >= a.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    for (d, s) in a.sun_path.iter_mut().zip(p) {
        *d = *s as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sa_family_t>() + p.len() + 1;
    Ok((a, len as libc::socklen_t))
}

/// Binds and listens on `path` (an old socket file there is removed first).
pub fn listen(path: &Path) -> io::Result<OwnedFd> {
    let fd = seqpacket()?;
    let (a, len) = address(path)?;
    let _ = std::fs::remove_file(path);
    // SAFETY: a is a valid sockaddr_un of length len.
    cvt(unsafe { libc::bind(fd.as_raw_fd(), (&raw const a).cast(), len) })?;
    // SAFETY: plain listen(2) on our socket.
    cvt(unsafe { libc::listen(fd.as_raw_fd(), 2) })?;
    Ok(fd)
}

/// Accepts one connection (blocking unless the listener is non-blocking).
pub fn accept(listener: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    // SAFETY: accept4 with no address out-parameters.
    let fd = cvt(unsafe {
        libc::accept4(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    })?;
    // SAFETY: fd was just returned by accept4.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub fn connect(path: &Path) -> io::Result<OwnedFd> {
    let fd = seqpacket()?;
    let (a, len) = address(path)?;
    // SAFETY: a is a valid sockaddr_un of length len.
    cvt(unsafe { libc::connect(fd.as_raw_fd(), (&raw const a).cast(), len) })?;
    Ok(fd)
}

/// `connect` that gives up after `timeout` when the listener's backlog is full (a helper that
/// is stopped or hung does not accept): a plain blocking connect would wait for ever.
pub fn connect_timeout(path: &Path, timeout: std::time::Duration) -> io::Result<OwnedFd> {
    let fd = seqpacket()?;
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: SO_SNDTIMEO with a timeval of the right size (it bounds connect(2) on AF_UNIX).
    cvt(unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            (&raw const tv).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    })?;
    let (a, len) = address(path)?;
    // SAFETY: a is a valid sockaddr_un of length len.
    cvt(unsafe { libc::connect(fd.as_raw_fd(), (&raw const a).cast(), len) })?;
    Ok(fd)
}

/// The pid of the peer process (`SO_PEERCRED`, as seen in the caller's pid namespace).
pub fn peer_pid(sock: BorrowedFd<'_>) -> io::Result<libc::pid_t> {
    // SAFETY: ucred is plain data.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED writes one ucred.
    cvt(unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    })?;
    Ok(cred.pid)
}

/// A connected pair (tests, or a helper started by the body itself).
pub fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: socketpair writes two fds into the array.
    cvt(unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    })?;
    // SAFETY: both fds were just created and are owned by us.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

pub fn set_nonblocking(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    // SAFETY: F_GETFL/F_SETFL on a valid fd.
    unsafe {
        let fl = cvt(libc::fcntl(fd.as_raw_fd(), libc::F_GETFL))?;
        let fl = if on {
            fl | libc::O_NONBLOCK
        } else {
            fl & !libc::O_NONBLOCK
        };
        cvt(libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl))?;
    }
    Ok(())
}

/// Ancillary data buffer large enough for `MAX_PLANES` fds, aligned for `cmsghdr`.
#[repr(C, align(8))]
struct Cmsg([u8; 64]);

const _: () = assert!(
    // CMSG_SPACE(4 * sizeof(int)) on 64-bit Linux: 16-byte header + 16 bytes.
    std::mem::size_of::<libc::cmsghdr>() + MAX_PLANES * 4 <= 64
);

/// Sends one message with its fds (`msg.fd_count()` of them). Never raises SIGPIPE.
pub fn send(sock: BorrowedFd<'_>, msg: &Message, fds: &[BorrowedFd<'_>]) -> Result<(), Error> {
    if fds.len() != msg.fd_count() {
        return Err(Error::Fds(fds.len(), msg.fd_count()));
    }
    let bytes = msg.encode()?;
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut _,
        iov_len: bytes.len(),
    };
    let mut cmsg = Cmsg([0; 64]);
    // SAFETY: msghdr is plain data; zeroed is valid.
    let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    if !fds.is_empty() {
        let data_len = std::mem::size_of_val(fds) as u32;
        // SAFETY: CMSG_* compute sizes and pointers inside `cmsg`, which is large enough
        // (asserted above) and aligned.
        unsafe {
            mh.msg_control = cmsg.0.as_mut_ptr().cast();
            mh.msg_controllen = libc::CMSG_SPACE(data_len) as _;
            let c = libc::CMSG_FIRSTHDR(&mh);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(data_len) as _;
            let p = libc::CMSG_DATA(c).cast::<RawFd>();
            for (i, fd) in fds.iter().enumerate() {
                p.add(i).write_unaligned(fd.as_raw_fd());
            }
        }
    }
    loop {
        // SAFETY: mh points at live buffers for the duration of the call.
        let r = unsafe { libc::sendmsg(sock.as_raw_fd(), &mh, libc::MSG_NOSIGNAL) };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e.into());
        }
    }
}

/// What `recv` got.
#[derive(Debug)]
pub enum Recv {
    Message(Message, Vec<OwnedFd>),
    /// The peer closed the connection.
    Eof,
}

/// Receives one message. `Err(Error::Io(WouldBlock))` on a non-blocking socket with nothing to
/// read; `Err(e)` with `e.is_unknown()` for a message type from a newer peer (skip it). The
/// fds of a bad message are closed.
pub fn recv(sock: BorrowedFd<'_>) -> Result<Recv, Error> {
    let mut buf = vec![0u8; MAX_MESSAGE];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut cmsg = Cmsg([0; 64]);
    // SAFETY: msghdr is plain data; zeroed is valid.
    let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cmsg.0.as_mut_ptr().cast();
    mh.msg_controllen = cmsg.0.len() as _;
    let n = loop {
        // SAFETY: mh points at live buffers for the duration of the call.
        let r = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut mh, libc::MSG_CMSG_CLOEXEC) };
        if r >= 0 {
            break r as usize;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e.into());
        }
    };
    let mut fds = Vec::new();
    // SAFETY: walking the control messages the kernel wrote into `cmsg`; every SCM_RIGHTS fd
    // is taken into an OwnedFd at once so none leaks.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&mh);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c);
                let len = (*c).cmsg_len as usize - (data as usize - c as usize);
                for i in 0..len / std::mem::size_of::<RawFd>() {
                    let fd = data.cast::<RawFd>().add(i).read_unaligned();
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            c = libc::CMSG_NXTHDR(&mh, c);
        }
    }
    if mh.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(Error::Truncated);
    }
    if n == 0 && fds.is_empty() {
        return Ok(Recv::Eof);
    }
    let msg = Message::decode(&buf[..n], fds.len())?;
    Ok(Recv::Message(msg, fds))
}

/// Sends a message that carries no fds.
impl Message {
    pub fn send_to(&self, sock: impl AsFd) -> Result<(), Error> {
        send(sock.as_fd(), self, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use std::io::{Read, Seek, Write};

    fn memfd(content: &[u8]) -> std::fs::File {
        // SAFETY: memfd_create with a static name.
        let fd = unsafe { libc::memfd_create(c"plane".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: fd is new and ours.
        let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
        f.write_all(content).unwrap();
        f
    }

    #[test]
    fn fds_cross_a_socketpair() {
        let (a, b) = pair().unwrap();
        let p0 = memfd(b"plane zero");
        let p1 = memfd(b"plane one");
        let msg = Message::Frame(Frame {
            buffer: 1,
            width: 4,
            height: 4,
            fourcc: FOURCC_XRGB8888,
            modifier: MOD_LINEAR,
            has_fds: true,
            seq: 1,
            time_ns: monotonic_ns(),
            planes: vec![
                Plane {
                    offset: 0,
                    stride: 16,
                },
                Plane {
                    offset: 64,
                    stride: 16,
                },
            ],
        });
        send(a.as_fd(), &msg, &[p0.as_fd(), p1.as_fd()]).unwrap();
        drop((p0, p1)); // the receiver's copies keep the files alive
        let Recv::Message(got, fds) = recv(b.as_fd()).unwrap() else {
            panic!("eof")
        };
        assert_eq!(got, msg);
        assert_eq!(fds.len(), 2);
        let mut s = String::new();
        let mut f = std::fs::File::from(fds.into_iter().nth(1).unwrap());
        f.rewind().unwrap();
        f.read_to_string(&mut s).unwrap();
        assert_eq!(s, "plane one");
    }

    #[test]
    fn every_message_crosses_and_eof() {
        let (a, b) = pair().unwrap();
        for m in crate::tests::all() {
            if m.fd_count() > 0 {
                continue;
            }
            m.send_to(&a).unwrap();
            match recv(b.as_fd()).unwrap() {
                Recv::Message(got, fds) => {
                    assert_eq!(got, m);
                    assert!(fds.is_empty());
                }
                Recv::Eof => panic!("eof"),
            }
        }
        set_nonblocking(b.as_fd(), true).unwrap();
        match recv(b.as_fd()) {
            Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::WouldBlock),
            other => panic!("{other:?}"),
        }
        drop(a);
        assert!(matches!(recv(b.as_fd()).unwrap(), Recv::Eof));
    }

    #[test]
    fn send_checks_the_fd_count() {
        let (a, _b) = pair().unwrap();
        let f = memfd(b"x");
        let r = send(a.as_fd(), &Message::Show, &[f.as_fd()]);
        assert!(matches!(r, Err(Error::Fds(1, 0))));
    }

    #[test]
    fn listen_and_connect() {
        let dir = std::env::temp_dir().join(format!("webview-proto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let l = listen(&path).unwrap();
        let c = connect(&path).unwrap();
        let s = accept(l.as_fd()).unwrap();
        Message::Reload.send_to(&c).unwrap();
        assert!(matches!(
            recv(s.as_fd()).unwrap(),
            Recv::Message(Message::Reload, _)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
