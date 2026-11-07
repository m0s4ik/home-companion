// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A tiny synchronous Wyoming client (what Home Assistant is to a satellite), used by
//! [`crate::fake_ha`] and the tests.

use std::io::BufReader;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::Error;
use crate::event::{encode_event, read_event, with_buffers};
use crate::message::Message;

pub struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    peer: SocketAddr,
}

impl Client {
    /// Connects with a timeout; `TCP_NODELAY` on.
    pub fn connect(addr: SocketAddr, timeout: Duration) -> Result<Client, Error> {
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Client {
            stream,
            reader,
            peer: addr,
        })
    }

    pub fn send(&mut self, m: &Message) -> Result<(), Error> {
        use std::io::Write;
        let e = m.to_event();
        with_buffers(|buf, scratch| {
            encode_event(buf, scratch, &e.kind, &e.data, e.payload.as_deref());
            self.stream.write_all(buf)?;
            self.stream.flush()?;
            Ok(())
        })
    }

    /// Next message; `Ok(None)` at EOF; `Err(Io(WouldBlock or TimedOut))` when `timeout` passes
    /// (`None` = wait forever). A timeout in the middle of a frame leaves the stream unusable.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Option<Message>, Error> {
        self.stream
            .set_read_timeout(timeout.map(|t| t.max(Duration::from_millis(1))))?;
        Ok(read_event(&mut self.reader)?.map(Message::from_event))
    }

    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// A clone of the socket (to write from another thread or to shut it down).
    pub fn try_clone_stream(&self) -> Result<TcpStream, Error> {
        Ok(self.stream.try_clone()?)
    }
}

/// True for the error [`Client::recv`] returns on timeout.
pub fn is_timeout(e: &Error) -> bool {
    matches!(e, Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}
