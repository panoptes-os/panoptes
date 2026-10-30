//! The desk's Wayland client of the Cage fork. Screencopy gives pixels, a
//! virtual pointer and keyboard give input, and panoptes_window_v1 gives
//! the window list.
//!
//! One thread reads and dispatches events for the life of the connection,
//! so the compositor never stalls on a client that stopped reading. Tools
//! send requests from the runtime's threads and await what the handlers
//! record:
//!
//! ```text
//! tool ───── request ─────────────────────> cage
//!   ^ oneshot: frame, sync                   │ events
//!   ^ watch:   window changes                v
//!   └──────────────── event thread: Handler ─> Shared
//! ```

use crate::keymap::KeyEvent;
use crate::protocol::{panoptes_window_manager_v1 as manager, panoptes_window_v1 as window};
use std::collections::BTreeMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};
use wayland_client::globals::{BindError, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as keyboard_manager, zwp_virtual_keyboard_v1 as keyboard,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as screencopy,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as pointer_manager, zwlr_virtual_pointer_v1 as pointer,
};

/// How long a plain capture or a sync may take before the compositor counts
/// as stuck.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// One window as the compositor last described it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// The desk's number for the window, in the order windows mapped.
    pub id: u32,
    pub pid: i32,
    pub app_id: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    /// The window this one is a dialog of.
    pub parent: Option<u32>,
}

/// The listed windows, in the order they mapped, and Xwayland's DISPLAY.
#[derive(Debug, Clone, Default)]
pub struct Windows {
    pub xwayland: Option<String>,
    pub list: Vec<Window>,
}

/// A captured frame as packed RGB, top row first.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

#[derive(Debug)]
pub struct Wl {
    conn: Connection,
    qh: QueueHandle<Handler>,
    output: wl_output::WlOutput,
    screencopy: screencopy::ZwlrScreencopyManagerV1,
    pointer: pointer::ZwlrVirtualPointerV1,
    keyboard: keyboard::ZwpVirtualKeyboardV1,
    shared: Arc<Shared>,
    /// Zero for input event timestamps.
    epoch: Instant,
}

/// What the event thread records for tools to read.
#[derive(Debug)]
struct Shared {
    windows: Mutex<WindowTable>,
    /// Marked at every window's done or closed, and when the event thread
    /// stops.
    changes: watch::Sender<()>,
    /// The output's current mode.
    size: Mutex<(u32, u32)>,
    /// Why the event thread stopped, once it has.
    lost: Mutex<Option<String>>,
}

impl Shared {
    fn changed(&self) {
        self.changes.send_replace(());
    }
}

#[derive(Debug, Default)]
struct WindowTable {
    xwayland: Option<String>,
    next_id: u32,
    entries: BTreeMap<u32, Entry>,
}

#[derive(Debug)]
struct Entry {
    handle: window::PanoptesWindowV1,
    /// Properties as they arrive.
    pending: Window,
    /// Properties at the last done; none before the first.
    shown: Option<Window>,
}

/// A window proxy's desk number, set when the manager announces it. The
/// compositor's object ids are reused after a window closes; these are not.
#[derive(Debug, Default)]
struct WindowId(OnceLock<u32>);

fn window_id(w: &window::PanoptesWindowV1) -> Option<u32> {
    w.data::<WindowId>()?.0.get().copied()
}

/// The event thread's dispatch state.
#[derive(Debug)]
struct Handler {
    shared: Arc<Shared>,
    shm: wl_shm::WlShm,
}

impl Wl {
    /// Connect to the compositor at `socket`, bind everything the desk
    /// drives, and start the event thread.
    pub fn connect(socket: &Path) -> Result<Wl, String> {
        let stream =
            UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
        let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<Handler>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(lacks("wl_shm"))?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).map_err(lacks("wl_seat"))?;
        let output: wl_output::WlOutput =
            globals.bind(&qh, 1..=1, ()).map_err(lacks("wl_output"))?;
        let screencopy: screencopy::ZwlrScreencopyManagerV1 = globals
            .bind(&qh, 3..=3, ())
            .map_err(lacks("zwlr_screencopy_manager_v1 3"))?;
        let pointers: pointer_manager::ZwlrVirtualPointerManagerV1 =
            globals
                .bind(&qh, 2..=2, ())
                .map_err(lacks("zwlr_virtual_pointer_manager_v1 2"))?;
        let keyboards: keyboard_manager::ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(lacks("zwp_virtual_keyboard_manager_v1"))?;
        let _windows: manager::PanoptesWindowManagerV1 =
            globals.bind(&qh, 1..=1, ()).map_err(|_| {
                "the compositor has no panoptes_window_manager_v1; is cage the Panoptes fork?"
                    .to_owned()
            })?;
        // The pointer moves over this output only, so absolute motion is in
        // its pixels.
        let pointer =
            pointers.create_virtual_pointer_with_output(Some(&seat), Some(&output), &qh, ());
        let keyboard = keyboards.create_virtual_keyboard(&seat, &qh, ());

        let shared = Arc::new(Shared {
            windows: Mutex::default(),
            changes: watch::channel(()).0,
            size: Mutex::default(),
            lost: Mutex::default(),
        });
        let mut handler = Handler {
            shared: shared.clone(),
            shm,
        };
        // Binding the window manager sends every window's state before the
        // roundtrip's reply.
        queue.roundtrip(&mut handler).map_err(|e| e.to_string())?;
        if *lock(&shared.size) == (0, 0) {
            return Err("the output reported no current mode".into());
        }
        std::thread::Builder::new()
            .name("wayland".into())
            .spawn(move || {
                let err = loop {
                    if let Err(e) = queue.blocking_dispatch(&mut handler) {
                        break e;
                    }
                };
                *lock(&handler.shared.lost) = Some(err.to_string());
                handler.shared.changed();
            })
            .map_err(|e| format!("event thread: {e}"))?;
        Ok(Wl {
            conn,
            qh,
            output,
            screencopy,
            pointer,
            keyboard,
            shared,
            epoch: Instant::now(),
        })
    }

    /// Fail once the connection to the compositor is gone.
    pub fn alive(&self) -> Result<(), String> {
        match lock(&self.shared.lost).as_ref() {
            Some(e) => Err(format!("lost the compositor: {e}")),
            None => Ok(()),
        }
    }

    /// The output's size in pixels: the space of screenshots and pointer
    /// coordinates.
    pub fn size(&self) -> (u32, u32) {
        *lock(&self.shared.size)
    }

    pub fn windows(&self) -> Windows {
        let table = lock(&self.shared.windows);
        Windows {
            xwayland: table.xwayland.clone(),
            list: table
                .entries
                .values()
                .filter_map(|e| e.shown.clone())
                .collect(),
        }
    }

    /// A receiver that changes at every window change.
    pub fn window_changes(&self) -> watch::Receiver<()> {
        self.shared.changes.subscribe()
    }

    /// Raise window `id` and give it keyboard focus.
    pub fn activate(&self, id: u32) -> Result<(), String> {
        self.handle(id)?.activate();
        self.flush()
    }

    /// Ask window `id`'s client to close it.
    pub fn close(&self, id: u32) -> Result<(), String> {
        self.handle(id)?.close();
        self.flush()
    }

    fn handle(&self, id: u32) -> Result<window::PanoptesWindowV1, String> {
        let table = lock(&self.shared.windows);
        match table.entries.get(&id) {
            Some(e) if e.shown.is_some() => Ok(e.handle.clone()),
            _ => Err(format!("no window {id}")),
        }
    }

    /// Capture the output at its next frame.
    pub async fn screenshot(&self) -> Result<Frame, String> {
        self.capture(false)?
            .wait(REPLY_TIMEOUT)
            .await?
            .ok_or_else(|| "the compositor sent no frame".into())
    }

    /// Capture the next frame that changes the screen since this client's
    /// last such capture, waiting at most `within`; `None` if nothing
    /// changed.
    pub async fn next_change(&self, within: Duration) -> Result<Option<Frame>, String> {
        self.capture(true)?.wait(within).await
    }

    fn capture(&self, damage: bool) -> Result<Pending, String> {
        let (tx, rx) = oneshot::channel();
        let capture = Arc::new(Capture {
            damage,
            state: Mutex::new(CaptureState {
                reply: Some(tx),
                ..CaptureState::default()
            }),
        });
        let frame = self
            .screencopy
            .capture_output(0, &self.output, &self.qh, capture.clone());
        self.flush()?;
        Ok(Pending { frame, capture, rx })
    }

    /// Wait until the compositor has handled every request sent so far.
    pub async fn sync(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.conn
            .display()
            .sync(&self.qh, SyncReply(Mutex::new(Some(tx))));
        self.flush()?;
        match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
            Ok(Ok(())) => Ok(()),
            _ => Err("the compositor did not answer a sync".into()),
        }
    }

    // Input requests queue until the caller flushes, so one action's events
    // reach the compositor together.

    fn time(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    /// Move the pointer to output pixel (x, y).
    pub fn motion(&self, x: u32, y: u32) {
        let (w, h) = self.size();
        self.pointer.motion_absolute(self.time(), x, y, w, h);
        self.pointer.frame();
    }

    /// Press or release a pointer button (BTN_LEFT, ...).
    pub fn button(&self, button: u32, pressed: bool) {
        let state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        self.pointer.button(self.time(), button, state);
        self.pointer.frame();
    }

    /// Turn the wheel by whole notches; positive is down or right.
    pub fn wheel(&self, dx: i32, dy: i32) {
        /// Pixels per notch, as libinput reports a wheel click.
        const NOTCH: f64 = 15.0;
        let t = self.time();
        self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
        for (axis, n) in [
            (wl_pointer::Axis::VerticalScroll, dy),
            (wl_pointer::Axis::HorizontalScroll, dx),
        ] {
            if n != 0 {
                self.pointer.axis_discrete(t, axis, NOTCH * f64::from(n), n);
            }
        }
        self.pointer.frame();
    }

    /// Replace the keyboard's keymap with xkb text and wait until the
    /// compositor has it, so the keys after it use it.
    pub async fn keymap(&self, text: &str) -> Result<(), String> {
        let fd = rustix::fs::memfd_create("panoptes-keymap", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| format!("memfd: {e}"))?;
        let file = File::from(fd);
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        file.write_all_at(&bytes, 0)
            .map_err(|e| format!("keymap: {e}"))?;
        // WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1
        self.keyboard.keymap(1, file.as_fd(), bytes.len() as u32);
        self.sync().await
    }

    /// Send key events. Keys need a keymap that binds them, and modifier
    /// state comes only from [`KeyEvent::Modifiers`].
    pub fn keys(&self, events: &[KeyEvent]) {
        for e in events {
            match *e {
                KeyEvent::Key { code, pressed } => {
                    self.keyboard.key(self.time(), code, u32::from(pressed))
                }
                KeyEvent::Modifiers(mask) => self.keyboard.modifiers(mask, 0, 0, 0),
            }
        }
    }

    pub fn flush(&self) -> Result<(), String> {
        self.conn
            .flush()
            .map_err(|e| format!("lost the compositor: {e}"))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn lacks(global: &'static str) -> impl FnOnce(BindError) -> String {
    move |e| format!("the compositor lacks {global}: {e}")
}

/// A capture in flight.
#[derive(Debug)]
struct Pending {
    frame: frame::ZwlrScreencopyFrameV1,
    capture: Arc<Capture>,
    rx: oneshot::Receiver<Result<Raw, String>>,
}

impl Pending {
    /// The frame, or `None` if it did not arrive within `within`; then the
    /// capture is abandoned.
    async fn wait(mut self, within: Duration) -> Result<Option<Frame>, String> {
        let reply = match tokio::time::timeout(within, &mut self.rx).await {
            Ok(reply) => reply,
            Err(_) => {
                // The handler replies under this lock, so after it either
                // the reply is in the channel or it never will be.
                let mut state = lock(&self.capture.state);
                state.abandoned = true;
                state.release(&self.frame);
                drop(state);
                match self.rx.try_recv() {
                    Ok(raw) => Ok(raw),
                    Err(_) => return Ok(None),
                }
            }
        };
        match reply {
            Ok(raw) => raw?.to_frame().map(Some),
            Err(_) => Err("lost the compositor during a capture".into()),
        }
    }
}

/// One screencopy request, filled in by the frame's handler.
#[derive(Debug)]
struct Capture {
    damage: bool,
    state: Mutex<CaptureState>,
}

#[derive(Debug, Default)]
struct CaptureState {
    offer: Option<Offer>,
    shm: Option<Shm>,
    y_invert: bool,
    reply: Option<oneshot::Sender<Result<Raw, String>>>,
    /// The tool stopped waiting and destroyed the frame.
    abandoned: bool,
}

/// The shm buffer the compositor asked for.
#[derive(Debug, Clone, Copy)]
struct Offer {
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

impl CaptureState {
    /// The copied pixels, once the frame is ready.
    fn read(&self) -> Result<Raw, String> {
        let (Some(offer), Some(shm)) = (self.offer, self.shm.as_ref()) else {
            return Err("a frame was ready before its buffer".into());
        };
        let mut data = vec![0; offer.stride as usize * offer.height as usize];
        shm.file
            .read_exact_at(&mut data, 0)
            .map_err(|e| format!("frame buffer: {e}"))?;
        Ok(Raw {
            offer,
            y_invert: self.y_invert,
            data,
        })
    }

    /// Destroy the frame, then its buffer.
    fn release(&mut self, frame: &frame::ZwlrScreencopyFrameV1) {
        frame.destroy();
        if let Some(shm) = self.shm.take() {
            shm.destroy();
        }
    }

    /// Release the frame and hand the tool its result.
    fn finish(&mut self, frame: &frame::ZwlrScreencopyFrameV1, result: Result<Raw, String>) {
        self.release(frame);
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(result);
        }
    }
}

/// A frame's pixels as the compositor wrote them.
#[derive(Debug)]
struct Raw {
    offer: Offer,
    y_invert: bool,
    data: Vec<u8>,
}

impl Raw {
    fn to_frame(&self) -> Result<Frame, String> {
        let Offer {
            format,
            width,
            height,
            stride,
        } = self.offer;
        // Little-endian 32-bit pixels: XRGB8888 is B, G, R, X in memory.
        let (r, g, b) = match format {
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => (2, 1, 0),
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => (0, 1, 2),
            other => return Err(format!("unsupported screencopy format {other:?}")),
        };
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for row in 0..h {
            let src = if self.y_invert { h - 1 - row } else { row };
            let line = &self.data[src * stride..src * stride + w * 4];
            for px in line.chunks_exact(4) {
                rgb.extend_from_slice(&[px[r], px[g], px[b]]);
            }
        }
        Ok(Frame { width, height, rgb })
    }
}

/// A shared-memory buffer the compositor copies a frame into.
#[derive(Debug)]
struct Shm {
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
}

impl Shm {
    fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<Handler>, offer: Offer) -> std::io::Result<Shm> {
        let Offer {
            format,
            width,
            height,
            stride,
        } = offer;
        let size = u64::from(stride) * u64::from(height);
        let fd = rustix::fs::memfd_create("panoptes-frame", rustix::fs::MemfdFlags::CLOEXEC)?;
        let file = File::from(fd);
        file.set_len(size)?;
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        Ok(Shm { file, pool, buffer })
    }

    fn destroy(self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

impl Dispatch<frame::ZwlrScreencopyFrameV1, Arc<Capture>> for Handler {
    fn event(
        state: &mut Self,
        proxy: &frame::ZwlrScreencopyFrameV1,
        event: frame::Event,
        capture: &Arc<Capture>,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let mut c = lock(&capture.state);
        if c.abandoned {
            return;
        }
        match event {
            frame::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                c.offer = Some(Offer {
                    format,
                    width,
                    height,
                    stride,
                });
            }
            frame::Event::Flags { flags } => {
                c.y_invert = matches!(flags, WEnum::Value(f) if f.contains(frame::Flags::YInvert));
            }
            frame::Event::BufferDone => {
                let shm = match c.offer {
                    Some(offer) => {
                        Shm::new(&state.shm, qh, offer).map_err(|e| format!("frame buffer: {e}"))
                    }
                    None => Err("the compositor offered no shm buffer".into()),
                };
                match shm {
                    Ok(shm) => {
                        if capture.damage {
                            proxy.copy_with_damage(&shm.buffer);
                        } else {
                            proxy.copy(&shm.buffer);
                        }
                        c.shm = Some(shm);
                    }
                    Err(e) => c.finish(proxy, Err(e)),
                }
            }
            frame::Event::Ready { .. } => {
                let raw = c.read();
                c.finish(proxy, raw);
            }
            frame::Event::Failed => {
                c.finish(proxy, Err("the compositor failed to copy a frame".into()));
            }
            _ => {}
        }
    }
}

impl Dispatch<manager::PanoptesWindowManagerV1, ()> for Handler {
    fn event(
        state: &mut Self,
        _: &manager::PanoptesWindowManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut table = lock(&state.shared.windows);
        match event {
            manager::Event::Xwayland { display } => table.xwayland = Some(display),
            manager::Event::Window { window } => {
                table.next_id += 1;
                let id = table.next_id;
                let slot = window.data::<WindowId>().expect("created with a WindowId");
                slot.0.set(id).expect("a new window has no id yet");
                table.entries.insert(
                    id,
                    Entry {
                        handle: window,
                        pending: Window {
                            id,
                            ..Window::default()
                        },
                        shown: None,
                    },
                );
            }
        }
    }

    event_created_child!(Handler, manager::PanoptesWindowManagerV1, [
        manager::EVT_WINDOW_OPCODE => (window::PanoptesWindowV1, WindowId::default()),
    ]);
}

impl Dispatch<window::PanoptesWindowV1, WindowId> for Handler {
    fn event(
        state: &mut Self,
        proxy: &window::PanoptesWindowV1,
        event: window::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The manager's window event, which sets the id, comes first.
        let id = *id.0.get().expect("window events follow its announcement");
        let mut table = lock(&state.shared.windows);
        let changed = match event {
            window::Event::Closed => {
                table.entries.remove(&id);
                proxy.destroy();
                true
            }
            event => {
                let entry = table
                    .entries
                    .get_mut(&id)
                    .expect("an open window has an entry");
                entry.apply(event)
            }
        };
        drop(table);
        if changed {
            state.shared.changed();
        }
    }
}

impl Entry {
    /// Record one event; true at done, which shows the pending state.
    fn apply(&mut self, event: window::Event) -> bool {
        let w = &mut self.pending;
        match event {
            window::Event::Pid { pid } => w.pid = pid,
            window::Event::AppId { app_id } => w.app_id = app_id,
            window::Event::Title { title } => w.title = title,
            window::Event::Geometry {
                x,
                y,
                width,
                height,
            } => (w.x, w.y, w.width, w.height) = (x, y, width, height),
            window::Event::Parent { parent } => w.parent = parent.as_ref().and_then(window_id),
            window::Event::Focused { focused } => w.focused = focused != 0,
            window::Event::Done => {
                self.shown = Some(self.pending.clone());
                return true;
            }
            window::Event::Closed => unreachable!("the caller drops closed windows"),
        }
        false
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Handler {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode {
            flags: WEnum::Value(flags),
            width,
            height,
            ..
        } = event
        {
            if flags.contains(wl_output::Mode::Current) {
                *lock(&state.shared.size) = (width as u32, height as u32);
            }
        }
    }
}

/// The sender a sync's callback completes.
#[derive(Debug)]
struct SyncReply(Mutex<Option<oneshot::Sender<()>>>);

impl Dispatch<wl_callback::WlCallback, SyncReply> for Handler {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        reply: &SyncReply,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(tx) = lock(&reply.0).take() {
            let _ = tx.send(());
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Handler {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Handler: ignore wl_seat::WlSeat);
delegate_noop!(Handler: ignore wl_shm::WlShm);
delegate_noop!(Handler: ignore wl_buffer::WlBuffer);
delegate_noop!(Handler: wl_shm_pool::WlShmPool);
delegate_noop!(Handler: screencopy::ZwlrScreencopyManagerV1);
delegate_noop!(Handler: pointer_manager::ZwlrVirtualPointerManagerV1);
delegate_noop!(Handler: pointer::ZwlrVirtualPointerV1);
delegate_noop!(Handler: keyboard_manager::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Handler: keyboard::ZwpVirtualKeyboardV1);
