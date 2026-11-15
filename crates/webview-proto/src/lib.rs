// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The protocol between the body (`home-companion`) and the web view helper (`home-companion-web`), over a
//! `SOCK_SEQPACKET` Unix socket: one message per packet, an 8-byte little-endian header
//! (type, flags, length) and a payload; file descriptors (the dma-buf planes of a frame) travel
//! as `SCM_RIGHTS`. The same layout is in `web/home-companion-web/protocol.h` for the C helper; the
//! reference is `web/PROTOCOL.md`.
//!
//! Compatibility: a receiver ignores message types it does not know and bytes after the
//! fields it knows, so later versions can add messages and append fields.

mod socket;

pub use socket::{
    Recv, accept, connect, connect_timeout, listen, pair, peer_pid, recv, send, set_nonblocking,
};

/// Protocol version, sent in `hello` by both sides.
/// 2: `pause` and `resume`.
pub const VERSION: u32 = 2;
/// Header size: type (u16), flags (u16), length of the whole message (u32).
pub const HEADER_SIZE: usize = 8;
/// Largest message, header included.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Planes (and fds) per frame.
pub const MAX_PLANES: usize = 4;

/// `DRM_FORMAT_MOD_INVALID`: the buffer has an implicit, driver-private layout.
pub const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
/// `DRM_FORMAT_MOD_LINEAR`.
pub const MOD_LINEAR: u64 = 0;

/// `fourcc_code(a, b, c, d)` from `drm_fourcc.h`.
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | (code[1] as u32) << 8 | (code[2] as u32) << 16 | (code[3] as u32) << 24
}
pub const FOURCC_ARGB8888: u32 = fourcc(b"AR24");
pub const FOURCC_XRGB8888: u32 = fourcc(b"XR24");
pub const FOURCC_ABGR8888: u32 = fourcc(b"AB24");
pub const FOURCC_XBGR8888: u32 = fourcc(b"XB24");

/// Message types (the header's first field).
pub mod kind {
    pub const HELLO: u16 = 1;
    pub const FRAME: u16 = 2;
    pub const DROP: u16 = 3;
    pub const RELEASE: u16 = 4;
    pub const PRESENTED: u16 = 5;
    pub const SHOW: u16 = 6;
    pub const HIDE: u16 = 7;
    pub const TOUCH: u16 = 8;
    pub const MENU: u16 = 9;
    pub const MENU_RESULT: u16 = 10;
    pub const LOAD: u16 = 11;
    pub const RELOAD: u16 = 12;
    pub const BACK: u16 = 13;
    pub const STATE: u16 = 14;
    pub const CONFIRM: u16 = 15;
    pub const CONFIRM_RESULT: u16 = 16;
    pub const GUARD: u16 = 17;
    pub const PAUSE: u16 = 18;
    pub const RESUME: u16 = 19;
}

/// Payload sizes of the fixed messages (header excluded).
pub mod size {
    pub const HELLO: usize = 16;
    pub const FRAME: usize = 80;
    pub const DROP: usize = 8;
    pub const RELEASE: usize = 8;
    pub const PRESENTED: usize = 24;
    pub const TOUCH: usize = 24;
    pub const MENU_RESULT: usize = 8;
    /// Fixed part of the variable messages.
    pub const MENU: usize = 28;
    pub const MENU_ITEM: usize = 8;
    pub const LOAD: usize = 4;
    pub const STATE: usize = 16;
    pub const CONFIRM: usize = 12;
    pub const CONFIRM_RESULT: usize = 8;
    pub const GUARD: usize = 4;
}

/// `frame.flags`: the message carries one fd per plane (first time this buffer id is sent).
pub const FRAME_FDS: u32 = 1;
/// `presented.flags`: the frame was not shown.
pub const PRESENTED_SKIPPED: u32 = 1;

/// `hello`: sent by both sides right after connecting. The body states the view size and scale
/// it wants; the helper answers with the ones it renders (frames may still differ in size
/// while a resize is in progress: each frame carries its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// Device scale × 1000 (1250 = 1.25).
    pub scale_milli: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plane {
    pub offset: u32,
    pub stride: u32,
}

/// `frame` (helper → body): a rendered buffer to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Assigned by the helper; stable while the buffer lives (until `drop`).
    pub buffer: u32,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    /// One fd per plane travels with the message (`FRAME_FDS`): only the first time an id is
    /// sent on this connection.
    pub has_fds: bool,
    /// Frame counter of the helper (for logs and latency).
    pub seq: u64,
    /// `CLOCK_MONOTONIC` nanoseconds when the helper got the frame.
    pub time_ns: u64,
    pub planes: Vec<Plane>,
}

/// `presented` (body → helper): the answer to every `frame`, exactly once. Normally sent when
/// the body's own frame that showed it reached the screen (page flip done); with `skipped`
/// when it never will (replaced by a newer frame before it was drawn, or its buffer dropped).
/// The helper paces rendering on it (WPE: `wpe_view_buffer_rendered`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presented {
    pub buffer: u32,
    pub skipped: bool,
    pub seq: u64,
    /// `CLOCK_MONOTONIC` nanoseconds of the flip.
    pub time_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TouchKind {
    Down = 0,
    Motion = 1,
    Up = 2,
    Cancel = 3,
}

/// `touch` (body → helper): one touch point changed. `x`, `y` in view pixels (the size of the
/// helper's `hello`, before the device scale), origin top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    pub kind: TouchKind,
    pub id: i32,
    pub x: f32,
    pub y: f32,
    /// `CLOCK_MONOTONIC` nanoseconds of the event.
    pub time_ns: u64,
}

/// `menu.items[].flags`
pub const MENU_ITEM_ENABLED: u32 = 1;
/// A group label (`<optgroup>`), not selectable.
pub const MENU_ITEM_GROUP: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub flags: u32,
    pub label: String,
}

/// `menu` (helper → body): a `<select>` was opened. The body draws the list and answers with
/// `menu-result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub menu: u32,
    /// Index of the selected item, -1 if none.
    pub selected: i32,
    /// The element's rectangle in view pixels.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LoadState {
    Loading = 0,
    Ready = 1,
    Error = 2,
}

/// `state` (helper → body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub state: LoadState,
    /// Load progress × 1000.
    pub progress_milli: u32,
    pub title: String,
    /// Error text (empty otherwise).
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello(Hello),
    Frame(Frame),
    /// helper → body: the buffer is gone (resize, web process restart); destroy its image.
    Drop {
        buffer: u32,
    },
    /// body → helper: the body will not sample the buffer again; the helper may reuse it.
    Release {
        buffer: u32,
    },
    Presented(Presented),
    Show,
    Hide,
    Touch(Touch),
    Menu(Menu),
    /// body → helper: chosen item, -1 = cancelled.
    MenuResult {
        menu: u32,
        index: i32,
    },
    Load {
        url: String,
    },
    Reload,
    Back,
    State(State),
    /// helper → body: the page wants to open a view the body guards (`guard`); the body asks
    /// the user and answers with `confirm-result`.
    Confirm(Confirm),
    /// body → helper: the answer to `confirm` (true = open it).
    ConfirmResult {
        id: u32,
        accepted: bool,
    },
    /// body → helper: path prefixes of the Home Assistant views that need a confirmation
    /// before they open (heavy views); replaces the previous list.
    Guard {
        paths: Vec<String>,
    },
    /// body → helper (version 2): stop the web process while the view stays hidden; the network
    /// process runs on, so the page's connections stay open.
    Pause,
    /// body → helper (version 2): let it run again (`show` and the body going away do too).
    Resume,
}

/// `confirm.kind`: a guarded (heavy) view.
pub const CONFIRM_HEAVY_VIEW: u32 = 0;

/// `confirm` (helper → body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub id: u32,
    pub kind: u32,
    /// The path the page wants to open (e.g. `/dashboard-3d/0`).
    pub path: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("message too short ({0} bytes)")]
    Short(usize),
    #[error("length field {field} does not match the packet ({packet} bytes)")]
    Length { field: usize, packet: usize },
    #[error("message too large ({0} bytes)")]
    TooLarge(usize),
    #[error("unknown message type {0}")]
    UnknownType(u16),
    #[error("bad field: {0}")]
    Field(&'static str),
    #[error("{0} fds for a message that needs {1}")]
    Fds(usize, usize),
    #[error("truncated packet or ancillary data")]
    Truncated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// A message from a newer peer: log once and carry on.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::UnknownType(_))
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// Bytes then zero padding to a multiple of 4.
    fn bytes_padded(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
        let pad = (4 - b.len() % 4) % 4;
        self.0.extend(std::iter::repeat_n(0, pad));
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Field("length"))?;
        let s = self.b.get(self.at..end).ok_or(Error::Short(self.b.len()))?;
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self, len: usize) -> Result<String, Error> {
        let pad = (4 - len % 4) % 4;
        let s = self.take(len)?;
        self.take(pad)?;
        String::from_utf8(s.to_vec()).map_err(|_| Error::Field("utf-8"))
    }
}

impl Message {
    pub fn kind(&self) -> u16 {
        use kind::*;
        match self {
            Message::Hello(_) => HELLO,
            Message::Frame(_) => FRAME,
            Message::Drop { .. } => DROP,
            Message::Release { .. } => RELEASE,
            Message::Presented(_) => PRESENTED,
            Message::Show => SHOW,
            Message::Hide => HIDE,
            Message::Touch(_) => TOUCH,
            Message::Menu(_) => MENU,
            Message::MenuResult { .. } => MENU_RESULT,
            Message::Load { .. } => LOAD,
            Message::Reload => RELOAD,
            Message::Back => BACK,
            Message::State(_) => STATE,
            Message::Confirm(_) => CONFIRM,
            Message::ConfirmResult { .. } => CONFIRM_RESULT,
            Message::Guard { .. } => GUARD,
            Message::Pause => PAUSE,
            Message::Resume => RESUME,
        }
    }

    /// Number of fds that must travel with this message.
    pub fn fd_count(&self) -> usize {
        match self {
            Message::Frame(f) if f.has_fds => f.planes.len(),
            _ => 0,
        }
    }

    /// The whole packet: header and payload.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer(Vec::with_capacity(HEADER_SIZE + size::FRAME));
        w.u16(self.kind());
        w.u16(0);
        w.u32(0); // length, patched below
        match self {
            Message::Hello(h) => {
                w.u32(h.version);
                w.u32(h.width);
                w.u32(h.height);
                w.u32(h.scale_milli);
            }
            Message::Frame(f) => {
                if f.planes.is_empty() || f.planes.len() > MAX_PLANES {
                    return Err(Error::Field("planes"));
                }
                w.u32(f.buffer);
                w.u32(f.width);
                w.u32(f.height);
                w.u32(f.fourcc);
                w.u64(f.modifier);
                w.u32(f.planes.len() as u32);
                w.u32(if f.has_fds { FRAME_FDS } else { 0 });
                w.u64(f.seq);
                w.u64(f.time_ns);
                for i in 0..MAX_PLANES {
                    let p = f.planes.get(i).copied().unwrap_or_default();
                    w.u32(p.offset);
                    w.u32(p.stride);
                }
            }
            Message::Drop { buffer } | Message::Release { buffer } => {
                w.u32(*buffer);
                w.u32(0);
            }
            Message::Presented(p) => {
                w.u32(p.buffer);
                w.u32(if p.skipped { PRESENTED_SKIPPED } else { 0 });
                w.u64(p.seq);
                w.u64(p.time_ns);
            }
            Message::Show
            | Message::Hide
            | Message::Reload
            | Message::Back
            | Message::Pause
            | Message::Resume => {}
            Message::Touch(t) => {
                w.u32(t.kind as u32);
                w.i32(t.id);
                w.f32(t.x);
                w.f32(t.y);
                w.u64(t.time_ns);
            }
            Message::Menu(m) => {
                w.u32(m.menu);
                w.i32(m.selected);
                w.i32(m.x);
                w.i32(m.y);
                w.i32(m.width);
                w.i32(m.height);
                w.u32(m.items.len() as u32);
                for it in &m.items {
                    w.u32(it.flags);
                    w.u32(it.label.len() as u32);
                    w.bytes_padded(it.label.as_bytes());
                }
            }
            Message::MenuResult { menu, index } => {
                w.u32(*menu);
                w.i32(*index);
            }
            Message::Load { url } => {
                w.u32(url.len() as u32);
                w.bytes_padded(url.as_bytes());
            }
            Message::State(s) => {
                w.u32(s.state as u32);
                w.u32(s.progress_milli);
                w.u32(s.title.len() as u32);
                w.u32(s.message.len() as u32);
                w.bytes_padded(s.title.as_bytes());
                w.bytes_padded(s.message.as_bytes());
            }
            Message::Confirm(c) => {
                w.u32(c.id);
                w.u32(c.kind);
                w.u32(c.path.len() as u32);
                w.bytes_padded(c.path.as_bytes());
            }
            Message::ConfirmResult { id, accepted } => {
                w.u32(*id);
                w.i32(i32::from(*accepted));
            }
            Message::Guard { paths } => {
                w.u32(paths.len() as u32);
                for p in paths {
                    w.u32(p.len() as u32);
                    w.bytes_padded(p.as_bytes());
                }
            }
        }
        let len = w.0.len();
        if len > MAX_MESSAGE {
            return Err(Error::TooLarge(len));
        }
        w.0[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        Ok(w.0)
    }

    /// One packet (as received) → message. `nfds` = fds that came with it, checked against
    /// the message.
    pub fn decode(packet: &[u8], nfds: usize) -> Result<Message, Error> {
        if packet.len() < HEADER_SIZE {
            return Err(Error::Short(packet.len()));
        }
        let ty = u16::from_le_bytes([packet[0], packet[1]]);
        let len = u32::from_le_bytes(packet[4..8].try_into().unwrap()) as usize;
        if len != packet.len() {
            return Err(Error::Length {
                field: len,
                packet: packet.len(),
            });
        }
        let body = &packet[HEADER_SIZE..];
        let need = |n: usize| {
            if body.len() < n {
                Err(Error::Short(packet.len()))
            } else {
                Ok(())
            }
        };
        let mut r = Reader { b: body, at: 0 };
        let msg = match ty {
            kind::HELLO => {
                need(size::HELLO)?;
                Message::Hello(Hello {
                    version: r.u32()?,
                    width: r.u32()?,
                    height: r.u32()?,
                    scale_milli: r.u32()?,
                })
            }
            kind::FRAME => {
                need(size::FRAME)?;
                let buffer = r.u32()?;
                let width = r.u32()?;
                let height = r.u32()?;
                let fourcc = r.u32()?;
                let modifier = r.u64()?;
                let n = r.u32()? as usize;
                let flags = r.u32()?;
                let seq = r.u64()?;
                let time_ns = r.u64()?;
                if n == 0 || n > MAX_PLANES {
                    return Err(Error::Field("plane count"));
                }
                let mut planes = Vec::with_capacity(n);
                for i in 0..MAX_PLANES {
                    let p = Plane {
                        offset: r.u32()?,
                        stride: r.u32()?,
                    };
                    if i < n {
                        planes.push(p);
                    }
                }
                Message::Frame(Frame {
                    buffer,
                    width,
                    height,
                    fourcc,
                    modifier,
                    has_fds: flags & FRAME_FDS != 0,
                    seq,
                    time_ns,
                    planes,
                })
            }
            kind::DROP | kind::RELEASE => {
                need(size::DROP)?;
                let buffer = r.u32()?;
                if ty == kind::DROP {
                    Message::Drop { buffer }
                } else {
                    Message::Release { buffer }
                }
            }
            kind::PRESENTED => {
                need(size::PRESENTED)?;
                let buffer = r.u32()?;
                let flags = r.u32()?;
                Message::Presented(Presented {
                    buffer,
                    skipped: flags & PRESENTED_SKIPPED != 0,
                    seq: r.u64()?,
                    time_ns: r.u64()?,
                })
            }
            kind::SHOW => Message::Show,
            kind::HIDE => Message::Hide,
            kind::RELOAD => Message::Reload,
            kind::BACK => Message::Back,
            kind::TOUCH => {
                need(size::TOUCH)?;
                let kind = match r.u32()? {
                    0 => TouchKind::Down,
                    1 => TouchKind::Motion,
                    2 => TouchKind::Up,
                    3 => TouchKind::Cancel,
                    _ => return Err(Error::Field("touch kind")),
                };
                Message::Touch(Touch {
                    kind,
                    id: r.i32()?,
                    x: r.f32()?,
                    y: r.f32()?,
                    time_ns: r.u64()?,
                })
            }
            kind::MENU => {
                need(size::MENU)?;
                let menu = r.u32()?;
                let selected = r.i32()?;
                let (x, y, width, height) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                let n = r.u32()? as usize;
                if n > MAX_MESSAGE / size::MENU_ITEM {
                    return Err(Error::Field("item count"));
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    let flags = r.u32()?;
                    let len = r.u32()? as usize;
                    items.push(MenuItem {
                        flags,
                        label: r.string(len)?,
                    });
                }
                Message::Menu(Menu {
                    menu,
                    selected,
                    x,
                    y,
                    width,
                    height,
                    items,
                })
            }
            kind::MENU_RESULT => {
                need(size::MENU_RESULT)?;
                Message::MenuResult {
                    menu: r.u32()?,
                    index: r.i32()?,
                }
            }
            kind::LOAD => {
                need(size::LOAD)?;
                let len = r.u32()? as usize;
                Message::Load {
                    url: r.string(len)?,
                }
            }
            kind::STATE => {
                need(size::STATE)?;
                let state = match r.u32()? {
                    0 => LoadState::Loading,
                    1 => LoadState::Ready,
                    2 => LoadState::Error,
                    _ => return Err(Error::Field("state")),
                };
                let progress_milli = r.u32()?;
                let (tl, ml) = (r.u32()? as usize, r.u32()? as usize);
                Message::State(State {
                    state,
                    progress_milli,
                    title: r.string(tl)?,
                    message: r.string(ml)?,
                })
            }
            kind::CONFIRM => {
                need(size::CONFIRM)?;
                let id = r.u32()?;
                let kind = r.u32()?;
                let len = r.u32()? as usize;
                Message::Confirm(Confirm {
                    id,
                    kind,
                    path: r.string(len)?,
                })
            }
            kind::CONFIRM_RESULT => {
                need(size::CONFIRM_RESULT)?;
                Message::ConfirmResult {
                    id: r.u32()?,
                    accepted: r.i32()? != 0,
                }
            }
            kind::GUARD => {
                need(size::GUARD)?;
                let n = r.u32()? as usize;
                if n > MAX_MESSAGE / 4 {
                    return Err(Error::Field("path count"));
                }
                let mut paths = Vec::with_capacity(n);
                for _ in 0..n {
                    let len = r.u32()? as usize;
                    paths.push(r.string(len)?);
                }
                Message::Guard { paths }
            }
            kind::PAUSE => Message::Pause,
            kind::RESUME => Message::Resume,
            other => return Err(Error::UnknownType(other)),
        };
        if nfds != msg.fd_count() {
            return Err(Error::Fds(nfds, msg.fd_count()));
        }
        Ok(msg)
    }
}

/// `CLOCK_MONOTONIC` in nanoseconds (the clock of every `time_ns` field).
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(has_fds: bool, planes: usize) -> Message {
        Message::Frame(Frame {
            buffer: 7,
            width: 1280,
            height: 800,
            fourcc: FOURCC_ARGB8888,
            modifier: MOD_INVALID,
            has_fds,
            seq: 1234,
            time_ns: 99,
            planes: (0..planes)
                .map(|i| Plane {
                    offset: i as u32 * 4096,
                    stride: 5120,
                })
                .collect(),
        })
    }

    pub(crate) fn all() -> Vec<Message> {
        vec![
            Message::Hello(Hello {
                version: VERSION,
                width: 1280,
                height: 800,
                scale_milli: 1250,
            }),
            frame(true, 1),
            frame(false, 2),
            Message::Drop { buffer: 3 },
            Message::Release { buffer: 4 },
            Message::Presented(Presented {
                buffer: 5,
                skipped: true,
                seq: 6,
                time_ns: 7,
            }),
            Message::Show,
            Message::Hide,
            Message::Touch(Touch {
                kind: TouchKind::Motion,
                id: 1,
                x: 12.5,
                y: 799.0,
                time_ns: 42,
            }),
            Message::Menu(Menu {
                menu: 9,
                selected: 1,
                x: 10,
                y: 20,
                width: 300,
                height: 40,
                items: vec![
                    MenuItem {
                        flags: MENU_ITEM_GROUP,
                        label: "Stanze".into(),
                    },
                    MenuItem {
                        flags: MENU_ITEM_ENABLED,
                        label: "Cucina è".into(),
                    },
                    MenuItem {
                        flags: 0,
                        label: String::new(),
                    },
                ],
            }),
            Message::MenuResult { menu: 9, index: -1 },
            Message::Load {
                url: "http://example.invalid:8123/lovelace/0".into(),
            },
            Message::Reload,
            Message::Back,
            Message::State(State {
                state: LoadState::Error,
                progress_milli: 500,
                title: "Home".into(),
                message: "connection refused".into(),
            }),
            Message::Confirm(Confirm {
                id: 3,
                kind: CONFIRM_HEAVY_VIEW,
                path: "/dashboard-3d/0".into(),
            }),
            Message::ConfirmResult {
                id: 3,
                accepted: true,
            },
            Message::Guard {
                paths: vec!["/dashboard-3d".into(), String::new(), "/x".into()],
            },
            Message::Guard { paths: vec![] },
            Message::Pause,
            Message::Resume,
        ]
    }

    #[test]
    fn round_trip_every_message() {
        for m in all() {
            let b = m.encode().unwrap();
            assert_eq!(b.len() % 4, 0, "{m:?} not 4-aligned");
            assert_eq!(Message::decode(&b, m.fd_count()).unwrap(), m);
        }
    }

    #[test]
    fn fixed_sizes() {
        let sizes = [
            (frame(true, 1), size::FRAME),
            (Message::Drop { buffer: 1 }, size::DROP),
            (Message::Show, 0),
            (all()[0].clone(), size::HELLO),
            (all()[5].clone(), size::PRESENTED),
            (all()[8].clone(), size::TOUCH),
            (Message::MenuResult { menu: 1, index: 0 }, size::MENU_RESULT),
            (
                Message::ConfirmResult {
                    id: 1,
                    accepted: false,
                },
                size::CONFIRM_RESULT,
            ),
            (Message::Guard { paths: vec![] }, size::GUARD),
        ];
        for (m, n) in sizes {
            assert_eq!(m.encode().unwrap().len(), HEADER_SIZE + n, "{m:?}");
        }
    }

    #[test]
    fn little_endian_layout() {
        let b = Message::Release {
            buffer: 0x0102_0304,
        }
        .encode()
        .unwrap();
        assert_eq!(b, [4, 0, 0, 0, 16, 0, 0, 0, 4, 3, 2, 1, 0, 0, 0, 0]);
        assert_eq!(FOURCC_XRGB8888, 0x3432_5258);
    }

    #[test]
    fn rejects_bad_packets() {
        let b = Message::Drop { buffer: 1 }.encode().unwrap();
        assert!(matches!(
            Message::decode(&b[..12], 0),
            Err(Error::Length { .. })
        ));
        assert!(matches!(Message::decode(&b[..4], 0), Err(Error::Short(4))));
        let mut u = b.clone();
        u[0] = 200;
        assert!(Message::decode(&u, 0).unwrap_err().is_unknown());
        let f = frame(true, 2).encode().unwrap();
        assert!(matches!(Message::decode(&f, 1), Err(Error::Fds(1, 2))));
        assert!(matches!(Message::decode(&b, 1), Err(Error::Fds(1, 0))));
    }

    #[test]
    fn accepts_appended_fields() {
        // A newer peer may append fields to a fixed message.
        let mut b = Message::Drop { buffer: 5 }.encode().unwrap();
        b.extend_from_slice(&[1, 2, 3, 4]);
        b[4] = b.len() as u8;
        assert_eq!(Message::decode(&b, 0).unwrap(), Message::Drop { buffer: 5 });
    }
}

#[cfg(test)]
mod golden {
    use super::*;

    /// The same bytes are checked from C by `web/protocol-test.c`.
    fn messages() -> Vec<(Message, &'static str)> {
        vec![
            (
                Message::Frame(Frame {
                    buffer: 7,
                    width: 1280,
                    height: 800,
                    fourcc: FOURCC_ARGB8888,
                    modifier: MOD_INVALID,
                    has_fds: true,
                    seq: 1234,
                    time_ns: 99,
                    planes: vec![Plane {
                        offset: 0,
                        stride: 5120,
                    }],
                }),
                "020000005800000007000000000500002003000041523234ffffffffffffff000100000001000000d20400000000000063000000000000000000000000140000000000000000000000000000000000000000000000000000",
            ),
            (
                Message::Touch(Touch {
                    kind: TouchKind::Motion,
                    id: 1,
                    x: 12.5,
                    y: 799.0,
                    time_ns: 42,
                }),
                "080000002000000001000000010000000000484100c047442a00000000000000",
            ),
            (
                Message::State(State {
                    state: LoadState::Error,
                    progress_milli: 500,
                    title: "Home".into(),
                    message: "connection refused".into(),
                }),
                "0e0000003000000002000000f40100000400000012000000486f6d65636f6e6e656374696f6e20726566757365640000",
            ),
            (
                Message::Guard {
                    paths: vec!["/dashboard-3d".into(), "/x".into()],
                },
                "1100000028000000020000000d0000002f64617368626f6172642d3364000000020000002f780000",
            ),
            (
                Message::Confirm(Confirm {
                    id: 3,
                    kind: CONFIRM_HEAVY_VIEW,
                    path: "/dashboard-3d/0".into(),
                }),
                "0f0000002400000003000000000000000f0000002f64617368626f6172642d33642f3000",
            ),
        ]
    }

    #[test]
    fn bytes_match_the_c_header() {
        for (m, hex) in messages() {
            let b = m.encode().unwrap();
            let got: String = b.iter().map(|x| format!("{x:02x}")).collect();
            assert_eq!(got, hex, "{m:?}");
        }
    }
}
