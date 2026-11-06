//! The daemon: tracks toplevels + MRU order while idle (event-driven, ~0% CPU), and builds the
//! switcher overlay on demand.
//!
//! Surface tree while open:
//!   root layer-shell surface (fullscreen, 1x1 backdrop buffer stretched by wp_viewport)
//!   ├── panel subsurface   (shm, CPU-drawn chrome; redrawn only on selection/title changes)
//!   └── preview subsurface × N (captured dmabufs, scaled by wp_viewport -> composited on the GPU)

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use wayland_client::backend::{ObjectId, WaylandError};
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::{self, WlSurface},
};
use wayland_client::{
    delegate_noop, event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use crate::buffers::{self, BusyFlag, GbmAllocator, Mmap};
use crate::capture::{Capture, Constraints, Slot};
use crate::config::{Config, Palette};
use crate::ipc::Command;
use crate::layout::{self, Layout, Rect};
use crate::render::{Item, Renderer};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Identifies a toplevel by its ext_foreign_toplevel_handle_v1 object id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub ObjectId);

// evdev key codes (layout independent, which is what we want for Tab/arrows).
mod key {
    pub const ESC: u32 = 1;
    pub const TAB: u32 = 15;
    pub const Q: u32 = 16;
    pub const W: u32 = 17;
    pub const ENTER: u32 = 28;
    pub const LEFTCTRL: u32 = 29;
    pub const H: u32 = 35;
    pub const J: u32 = 36;
    pub const K: u32 = 37;
    pub const L: u32 = 38;
    pub const LEFTSHIFT: u32 = 42;
    pub const RIGHTSHIFT: u32 = 54;
    pub const LEFTALT: u32 = 56;
    pub const SPACE: u32 = 57;
    pub const KPENTER: u32 = 96;
    pub const RIGHTCTRL: u32 = 97;
    pub const RIGHTALT: u32 = 100;
    pub const HOME: u32 = 102;
    pub const UP: u32 = 103;
    pub const LEFT: u32 = 105;
    pub const RIGHT: u32 = 106;
    pub const END: u32 = 107;
    pub const DOWN: u32 = 108;
    pub const DELETE: u32 = 111;
    pub const LEFTMETA: u32 = 125;
    pub const RIGHTMETA: u32 = 126;
    pub const HOLD_KEYS: [u32; 6] = [LEFTALT, RIGHTALT, LEFTCTRL, RIGHTCTRL, LEFTMETA, RIGHTMETA];
}
const MOD_SHIFT: u32 = 1 << 0;
const MOD_CTRL: u32 = 1 << 2;
const MOD_ALT: u32 = 1 << 3;
const MOD_SUPER: u32 = 1 << 6;
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

pub struct Toplevel {
    pub ext: ExtForeignToplevelHandleV1,
    pub cosmic: Option<ZcosmicToplevelHandleV1>,
    pub title: String,
    pub app_id: String,
    pub activated: bool,
    pub minimized: bool,
    pub size: Option<(i32, i32)>,
}

struct ShmBuf {
    buffer: WlBuffer,
    map: Mmap,
    width: u32,
    height: u32,
    busy: BusyFlag,
}

struct Panel {
    surface: WlSurface,
    sub: WlSubsurface,
    viewport: WpViewport,
    bufs: Vec<ShmBuf>,
}

struct Entry {
    key: Key,
    surface: WlSurface,
    sub: WlSubsurface,
    viewport: WpViewport,
    cap: Option<Capture>,
    /// Preview rect in root coordinates (logical px).
    placed: Option<Rect>,
}

struct Overlay {
    root: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    root_viewport: WpViewport,
    fractional: Option<WpFractionalScaleV1>,
    backdrop: Option<WlBuffer>,
    panel: Panel,
    entries: Vec<Entry>,
    configured: bool,
    size: (i32, i32),
    scale120: Option<u32>,
    int_scale: i32,
    layout: Option<Layout>,
    selected: usize,
    close_hover: bool,
    visible: bool,
    show_at: Instant,
    sticky: bool,
    has_focus: bool,
    pressed: HashSet<u32>,
    mods: u32,
    needs_layout: bool,
    needs_render: bool,
    scroll_acc: f64,
}

impl Overlay {
    fn scale(&self) -> f32 {
        match self.scale120 {
            Some(s) => s as f32 / 120.0,
            None => self.int_scale.max(1) as f32,
        }
    }

    fn holding(&self) -> bool {
        self.mods & (MOD_ALT | MOD_CTRL | MOD_SUPER) != 0 || key::HOLD_KEYS.iter().any(|k| self.pressed.contains(k))
    }

    fn shift(&self) -> bool {
        self.mods & MOD_SHIFT != 0 || self.pressed.contains(&key::LEFTSHIFT) || self.pressed.contains(&key::RIGHTSHIFT)
    }

    fn select(&mut self, idx: usize) {
        if idx < self.entries.len() && idx != self.selected {
            self.selected = idx;
            self.close_hover = false;
            self.needs_render = true;
        }
    }

    fn step(&mut self, delta: isize) {
        let n = self.entries.len() as isize;
        if n > 0 {
            self.select((self.selected as isize + delta).rem_euclid(n) as usize);
        }
    }
}

pub struct App {
    qh: QueueHandle<App>,
    compositor: WlCompositor,
    subcompositor: WlSubcompositor,
    shm: WlShm,
    shm_abgr: bool,
    viewporter: WpViewporter,
    layer_shell: ZwlrLayerShellV1,
    fractional_mgr: Option<WpFractionalScaleManagerV1>,
    dmabuf: Option<ZwpLinuxDmabufV1>,
    cursor_shape_mgr: Option<WpCursorShapeManagerV1>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    cursor_device: Option<WpCursorShapeDeviceV1>,
    pointer_pos: Option<(f32, f32)>,
    _toplevel_list: ExtForeignToplevelListV1,
    cosmic_info: ZcosmicToplevelInfoV1,
    cosmic_manager: ZcosmicToplevelManagerV1,
    capture_mgr: Option<ExtImageCopyCaptureManagerV1>,
    source_mgr: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,

    toplevels: HashMap<Key, Toplevel>,
    /// Most-recently-used order, front = currently/last focused.
    mru: Vec<Key>,

    cfg: Config,
    palette: Palette,
    renderer: Renderer,
    gbm: HashMap<u64, Option<GbmAllocator>>,
    overlay: Option<Overlay>,
    running: bool,
}

pub fn run(listener: UnixListener) -> Res<()> {
    let conn = Connection::connect_to_env().map_err(|e| format!("cannot connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut app = App::new(&globals, &qh)?;
    queue.roundtrip(&mut app)?;
    log::info!("tracking {} windows; waiting for commands", app.toplevels.len());
    app.event_loop(&conn, &mut queue, &listener)
}

fn need<I: Proxy + 'static>(
    globals: &GlobalList,
    qh: &QueueHandle<App>,
    versions: std::ops::RangeInclusive<u32>,
    what: &str,
) -> Res<I>
where
    App: Dispatch<I, ()>,
{
    globals
        .bind::<I, App, ()>(qh, versions, ())
        .map_err(|e| format!("compositor lacks {} ({what}): {e}. Is this COSMIC?", I::interface().name).into())
}

impl App {
    fn new(globals: &GlobalList, qh: &QueueHandle<App>) -> Res<Self> {
        let compositor: WlCompositor = need(globals, qh, 4..=6, "surfaces")?;
        let subcompositor: WlSubcompositor = need(globals, qh, 1..=1, "subsurfaces")?;
        let shm: WlShm = need(globals, qh, 1..=1, "shared memory")?;
        let viewporter: WpViewporter = need(globals, qh, 1..=1, "scaled previews")?;
        let layer_shell: ZwlrLayerShellV1 = need(globals, qh, 3..=4, "overlay surface")?;
        let toplevel_list: ExtForeignToplevelListV1 = need(globals, qh, 1..=1, "window list")?;
        let cosmic_info: ZcosmicToplevelInfoV1 = need(globals, qh, 2..=3, "window state")?;
        let cosmic_manager: ZcosmicToplevelManagerV1 = need(globals, qh, 1..=4, "window activation")?;
        let seat: Option<WlSeat> = globals.bind(qh, 1..=8, ()).ok();
        let fractional_mgr = globals.bind(qh, 1..=1, ()).ok();
        let dmabuf = globals.bind(qh, 3..=4, ()).ok();
        let cursor_shape_mgr = globals.bind(qh, 1..=1, ()).ok();
        let capture_mgr: Option<ExtImageCopyCaptureManagerV1> = globals.bind(qh, 1..=1, ()).ok();
        let source_mgr: Option<ExtForeignToplevelImageCaptureSourceManagerV1> = globals.bind(qh, 1..=1, ()).ok();
        if capture_mgr.is_none() || source_mgr.is_none() {
            log::warn!("compositor lacks ext-image-copy-capture / toplevel capture sources: previews disabled (icons only)");
        }
        if dmabuf.is_none() {
            log::warn!("compositor lacks linux-dmabuf: previews will use slow shared-memory capture");
        }
        let cfg = Config::load();
        let palette = Palette::resolve(&cfg);
        let renderer = Renderer::new(cfg.icon_theme.clone());
        Ok(Self {
            qh: qh.clone(),
            compositor,
            subcompositor,
            shm,
            shm_abgr: false,
            viewporter,
            layer_shell,
            fractional_mgr,
            dmabuf,
            cursor_shape_mgr,
            seat,
            keyboard: None,
            pointer: None,
            cursor_device: None,
            pointer_pos: None,
            _toplevel_list: toplevel_list,
            cosmic_info,
            cosmic_manager,
            capture_mgr,
            source_mgr,
            toplevels: HashMap::new(),
            mru: Vec::new(),
            cfg,
            palette,
            renderer,
            gbm: HashMap::new(),
            overlay: None,
            running: true,
        })
    }

    fn event_loop(&mut self, conn: &Connection, queue: &mut EventQueue<App>, listener: &UnixListener) -> Res<()> {
        listener.set_nonblocking(true)?;
        while self.running {
            queue.dispatch_pending(self)?;
            self.update();
            conn.flush()?;
            let Some(guard) = queue.prepare_read() else { continue };
            let timeout = self.next_deadline().map(|d| {
                let d = d.saturating_duration_since(Instant::now());
                Timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ }
            });
            let (wl_ready, ipc_ready) = {
                let wl_fd = guard.connection_fd();
                let mut fds = [PollFd::new(&wl_fd, PollFlags::IN), PollFd::new(listener, PollFlags::IN)];
                match poll(&mut fds, timeout.as_ref()) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(e) => return Err(e.into()),
                }
                let bad = PollFlags::ERR | PollFlags::HUP;
                (fds[0].revents().intersects(PollFlags::IN | bad), fds[1].revents().contains(PollFlags::IN))
            };
            if wl_ready {
                match guard.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(format!("wayland connection lost: {e}").into()),
                }
            } else {
                drop(guard);
            }
            if ipc_ready {
                self.accept_ipc(listener);
            }
        }
        self.hide();
        conn.flush()?;
        Ok(())
    }

    fn accept_ipc(&mut self, listener: &UnixListener) {
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            let mut buf = [0u8; 64];
            let mut len = 0;
            while len < buf.len() {
                match stream.read(&mut buf[len..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        len += n;
                        if buf[..len].contains(&b'\n') {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf[..len]);
            for line in text.lines() {
                match Command::parse(line.trim()) {
                    Some(cmd) => self.command(cmd),
                    None => log::warn!("unknown command {line:?}"),
                }
            }
        }
    }

    fn command(&mut self, cmd: Command) {
        log::debug!("command {cmd:?}");
        match cmd {
            Command::Next => self.show(1, false),
            Command::Prev => self.show(-1, false),
            Command::Show => self.show(1, true),
            Command::Toggle => {
                if self.overlay.is_some() {
                    self.hide()
                } else {
                    self.show(1, true)
                }
            }
            Command::Hide => self.hide(),
            Command::Quit => self.running = false,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let ov = self.overlay.as_ref()?;
        let now = Instant::now();
        let mut d: Option<Instant> = None;
        let mut consider = |t: Instant| {
            if t > now {
                d = Some(d.map_or(t, |x: Instant| x.min(t)));
            }
        };
        if ov.configured && !ov.visible {
            consider(ov.show_at);
        }
        for e in &ov.entries {
            if let Some(t) = e.cap.as_ref().filter(|c| c.frame.is_none() && !c.stopped).and_then(|c| c.next_at) {
                consider(t);
            }
        }
        d
    }

    // ------------------------------------------------------------------------------------------
    // Overlay lifecycle
    // ------------------------------------------------------------------------------------------

    fn show(&mut self, dir: isize, sticky: bool) {
        if let Some(ov) = self.overlay.as_mut() {
            ov.sticky |= sticky;
            if !sticky {
                ov.step(dir);
            }
            return;
        }
        self.cfg = Config::load();
        self.palette = Palette::resolve(&self.cfg);
        self.renderer.icons.set_theme(self.cfg.icon_theme.clone());
        self.renderer.icons.refresh_desktop_index();

        let keys: Vec<Key> = self
            .mru
            .iter()
            .filter(|k| self.toplevels.get(k).is_some_and(|t| self.cfg.include_minimized || !t.minimized))
            .cloned()
            .collect();
        if keys.is_empty() {
            log::info!("no windows to switch to");
            return;
        }
        let qh = &self.qh;

        let root = self.compositor.create_surface(qh, ());
        let layer = self.layer_shell.get_layer_surface(
            &root,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "pivot".into(),
            qh,
            (),
        );
        use zwlr_layer_surface_v1::Anchor;
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive);
        let root_viewport = self.viewporter.get_viewport(&root, qh, ());
        let fractional = self.fractional_mgr.as_ref().map(|m| m.get_fractional_scale(&root, qh, ()));

        let empty = self.compositor.create_region(qh, ());
        let mk_child = |app: &App| {
            let surface = app.compositor.create_surface(qh, ());
            let sub = app.subcompositor.get_subsurface(&surface, &root, qh, ());
            sub.set_desync();
            surface.set_input_region(Some(&empty));
            let viewport = app.viewporter.get_viewport(&surface, qh, ());
            (surface, sub, viewport)
        };
        // Panel first so previews stack above it.
        let (ps, psub, pvp) = mk_child(self);
        let panel = Panel { surface: ps, sub: psub, viewport: pvp, bufs: Vec::new() };
        let mut entries = Vec::with_capacity(keys.len());
        for k in keys {
            let (surface, sub, viewport) = mk_child(self);
            let cap = self.start_capture(&k);
            entries.push(Entry { key: k, surface, sub, viewport, cap, placed: None });
        }
        empty.destroy();
        root.commit();

        let n = entries.len();
        let selected = if n < 2 { 0 } else if dir < 0 { n - 1 } else { 1 };
        let delay = if sticky { 0 } else { self.cfg.show_delay_ms };
        self.overlay = Some(Overlay {
            root,
            layer,
            root_viewport,
            fractional,
            backdrop: None,
            panel,
            entries,
            configured: false,
            size: (0, 0),
            scale120: None,
            int_scale: 1,
            layout: None,
            selected,
            close_hover: false,
            visible: false,
            show_at: Instant::now() + Duration::from_millis(delay),
            sticky,
            has_focus: false,
            pressed: HashSet::new(),
            mods: 0,
            needs_layout: true,
            needs_render: true,
            scroll_acc: 0.0,
        });
    }

    fn start_capture(&self, k: &Key) -> Option<Capture> {
        let (mgr, src_mgr) = (self.capture_mgr.as_ref()?, self.source_mgr.as_ref()?);
        let t = self.toplevels.get(k)?;
        let source = src_mgr.create_source(&t.ext, &self.qh, ());
        let opts = if self.cfg.paint_cursors {
            ext_image_copy_capture_manager_v1::Options::PaintCursors
        } else {
            ext_image_copy_capture_manager_v1::Options::empty()
        };
        let session = mgr.create_session(&source, opts, &self.qh, k.clone());
        Some(Capture::new(source, session))
    }

    fn hide(&mut self) {
        let Some(ov) = self.overlay.take() else { return };
        for e in ov.entries {
            destroy_entry(e);
        }
        ov.panel.viewport.destroy();
        ov.panel.sub.destroy();
        ov.panel.surface.destroy();
        for b in ov.panel.bufs {
            b.buffer.destroy();
        }
        ov.root_viewport.destroy();
        if let Some(f) = ov.fractional {
            f.destroy();
        }
        ov.layer.destroy();
        ov.root.destroy();
        if let Some(b) = ov.backdrop {
            b.destroy();
        }
        self.renderer.trim();
    }

    fn activate_selected(&mut self) {
        let Some(key) = self.overlay.as_ref().and_then(|ov| ov.entries.get(ov.selected)).map(|e| e.key.clone()) else {
            self.hide();
            return;
        };
        self.hide();
        if let (Some(t), Some(seat)) = (self.toplevels.get(&key), self.seat.as_ref()) {
            if let Some(c) = &t.cosmic {
                self.cosmic_manager.activate(c, seat);
                // Optimistic MRU bump; the compositor's activated state confirms it shortly.
                self.touch_mru(&key);
            }
        }
    }

    fn close_selected(&mut self, idx: Option<usize>) {
        let Some(ov) = self.overlay.as_ref() else { return };
        let Some(e) = ov.entries.get(idx.unwrap_or(ov.selected)) else { return };
        if let Some(c) = self.toplevels.get(&e.key).and_then(|t| t.cosmic.as_ref()) {
            // The entry disappears when the compositor reports the window closed.
            self.cosmic_manager.close(c);
        }
    }

    fn touch_mru(&mut self, key: &Key) {
        if let Some(i) = self.mru.iter().position(|k| k == key) {
            let k = self.mru.remove(i);
            self.mru.insert(0, k);
        }
    }

    fn remove_entry(&mut self, key: &Key) {
        let Some(ov) = self.overlay.as_mut() else { return };
        let Some(i) = ov.entries.iter().position(|e| &e.key == key) else { return };
        let e = ov.entries.remove(i);
        destroy_entry(e);
        if ov.entries.is_empty() {
            self.hide();
            return;
        }
        if ov.selected > i || ov.selected >= ov.entries.len() {
            ov.selected = ov.selected.saturating_sub(1);
        }
        ov.needs_layout = true;
    }

    /// Runs after every batch of events: show/layout/render/capture scheduling.
    fn update(&mut self) {
        let Some(mut ov) = self.overlay.take() else { return };
        let now = Instant::now();

        // Captures negotiate in parallel with the layer-surface configure roundtrip.
        if !ov.configured {
            self.pump_captures(&mut ov, now);
            self.overlay = Some(ov);
            return;
        }
        if !ov.visible && now >= ov.show_at {
            ov.visible = true;
            ov.needs_layout = true;
            ov.needs_render = true;
        }
        if ov.needs_layout {
            self.apply_layout(&mut ov);
        }
        if ov.visible {
            if ov.needs_render {
                ov.needs_render = false;
                self.render_panel(&mut ov);
            }
            // Present frames captured before the panel was visible.
            for e in &mut ov.entries {
                if let Some(cap) = e.cap.as_mut() {
                    if let Some(slot) = cap.pending.take() {
                        present(e.surface.clone(), &e.viewport, e.placed, cap, slot);
                    }
                }
            }
        }
        self.pump_captures(&mut ov, now);
        self.overlay = Some(ov);
    }

    fn apply_layout(&mut self, ov: &mut Overlay) {
        ov.needs_layout = false;
        let aspects: Vec<f32> = ov
            .entries
            .iter()
            .map(|e| {
                e.cap
                    .as_ref()
                    .and_then(Capture::aspect)
                    .or_else(|| {
                        let (w, h) = self.toplevels.get(&e.key)?.size?;
                        (w > 0 && h > 0).then(|| w as f32 / h as f32)
                    })
                    .unwrap_or(16.0 / 9.0)
            })
            .collect();
        let l = layout::compute(&self.cfg, ov.size.0 as f32, ov.size.1 as f32, &aspects);
        if ov.layout.as_ref().is_some_and(|old| old.panel == l.panel && old.cells.iter().zip(&l.cells).all(|(a, b)| a.thumb == b.thumb) && old.cells.len() == l.cells.len()) {
            return;
        }
        let b = crate::render::bleed(&self.cfg);
        ov.panel.sub.set_position((l.panel.x - b) as i32, (l.panel.y - b) as i32);
        for (e, cell) in ov.entries.iter_mut().zip(&l.cells) {
            let r = cell.thumb.offset(l.panel.x, l.panel.y);
            e.sub.set_position(r.x as i32, r.y as i32);
            if e.placed.map(|p| (p.w, p.h)) != Some((r.w, r.h)) {
                e.viewport.set_destination(r.w as i32, r.h as i32);
                e.surface.commit();
            }
            e.placed = Some(r);
        }
        ov.root.commit();
        ov.layout = Some(l);
        ov.needs_render = true;
    }

    fn render_panel(&mut self, ov: &mut Overlay) {
        let Some(l) = ov.layout.as_ref() else { return };
        let items: Vec<Item> = ov
            .entries
            .iter()
            .map(|e| {
                let t = self.toplevels.get(&e.key);
                Item {
                    title: t.map(|t| t.title.as_str()).unwrap_or(""),
                    app_id: t.map(|t| t.app_id.as_str()).unwrap_or(""),
                    has_preview: e.cap.as_ref().is_some_and(|c| c.has_frame && !c.stopped),
                }
            })
            .collect();
        let scale = ov.scale();
        let Some(pm) = self.renderer.render(&self.cfg, &self.palette, l, &items, ov.selected, ov.close_hover, scale)
        else {
            return;
        };
        let (w, h) = (pm.width(), pm.height());
        ov.panel.bufs.retain(|b| {
            let keep = b.busy.get() || (b.width == w && b.height == h);
            if !keep {
                b.buffer.destroy();
            }
            keep
        });
        let idx = match ov.panel.bufs.iter().position(|b| !b.busy.get()) {
            Some(i) => i,
            None => match self.create_shm_buf(w, h) {
                Some(b) => {
                    ov.panel.bufs.push(b);
                    ov.panel.bufs.len() - 1
                }
                None => return,
            },
        };
        let buf = &mut ov.panel.bufs[idx];
        let dst = buf.map.as_mut_slice();
        let src = pm.data();
        if self.shm_abgr {
            dst[..src.len()].copy_from_slice(src);
        } else {
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        }
        ov.panel.surface.attach(Some(&buf.buffer), 0, 0);
        ov.panel.surface.damage_buffer(0, 0, w as i32, h as i32);
        let b = crate::render::bleed(&self.cfg);
        ov.panel.viewport.set_destination((l.panel.w + 2.0 * b) as i32, (l.panel.h + 2.0 * b) as i32);
        ov.panel.surface.commit();
        buf.busy.set(true);
    }

    fn create_shm_buf(&self, w: u32, h: u32) -> Option<ShmBuf> {
        let stride = w * 4;
        let size = (stride * h) as usize;
        let (fd, map) = buffers::shm_file(size, true).map_err(|e| log::error!("shm alloc: {e}")).ok()?;
        let busy = BusyFlag::default();
        let pool = self.shm.create_pool(fd.as_fd(), size as i32, &self.qh, ());
        let format = if self.shm_abgr { wl_shm::Format::Abgr8888 } else { wl_shm::Format::Argb8888 };
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &self.qh, busy.clone());
        pool.destroy();
        Some(ShmBuf { buffer, map: map?, width: w, height: h, busy })
    }

    fn ensure_backdrop(&mut self, ov: &mut Overlay) {
        if ov.backdrop.is_some() {
            return;
        }
        let Ok((fd, map)) = buffers::shm_file(4, true) else { return };
        if let Some(mut m) = map {
            m.as_mut_slice().copy_from_slice(&self.palette.backdrop.to_argb_premul().to_ne_bytes());
        }
        let pool = self.shm.create_pool(fd.as_fd(), 4, &self.qh, ());
        let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, &self.qh, BusyFlag::default());
        pool.destroy();
        ov.root.attach(Some(&buffer), 0, 0);
        ov.root.damage_buffer(0, 0, 1, 1);
        ov.backdrop = Some(buffer);
    }

    // ------------------------------------------------------------------------------------------
    // Capture scheduling
    // ------------------------------------------------------------------------------------------

    fn pump_captures(&mut self, ov: &mut Overlay, now: Instant) {
        let visible = ov.visible;
        let period = (self.cfg.max_fps > 0).then(|| Duration::from_secs_f64(1.0 / self.cfg.max_fps as f64));
        for e in &mut ov.entries {
            let Some(cap) = e.cap.as_mut() else { continue };
            cap.collect_stale();
            if cap.stopped || cap.frame.is_some() {
                continue;
            }
            // Before the panel shows, grab exactly one frame so it appears instantly with content.
            if !visible && (cap.pending.is_some() || cap.has_frame) {
                continue;
            }
            if let Some(t) = cap.next_at {
                if now < t {
                    continue;
                }
                cap.next_at = None;
            }
            let Some(c) = cap.constraints.clone() else { continue };
            if c.width == 0 || c.height == 0 {
                continue;
            }
            let slot = match cap.free_slot() {
                Some(i) => i,
                None if cap.live_slots() < self.cfg.capture_buffers => {
                    match self.alloc_slot(&e.key, &c, cap.force_shm) {
                        Some(s) => cap.insert_slot(s),
                        None => {
                            cap.stopped = true;
                            continue;
                        }
                    }
                }
                // All buffers busy: a wl_buffer.release will wake us up.
                None => continue,
            };
            let s = cap.slots[slot].as_ref().unwrap();
            let frame = cap.session.create_frame(&self.qh, e.key.clone());
            frame.attach_buffer(&s.buffer);
            frame.damage_buffer(0, 0, s.width as i32, s.height as i32);
            frame.capture();
            cap.frame = Some((frame, slot));
            cap.last_start = Some(now);
            if let Some(p) = period {
                cap.next_at = Some(now + p);
            }
        }
    }

    fn alloc_slot(&mut self, key: &Key, c: &Constraints, force_shm: bool) -> Option<Slot> {
        let busy = BusyFlag::default();
        let (w, h) = (c.width, c.height);
        if let (false, false, Some(dmabuf), Some(dev)) = (force_shm, self.cfg.force_shm, self.dmabuf.as_ref(), c.dmabuf_device) {
            let gbm = self.gbm.entry(dev).or_insert_with(|| {
                GbmAllocator::open(dev).map_err(|e| log::warn!("GBM unavailable ({e}); using shared memory")).ok()
            });
            if let (Some(gbm), Some((fourcc, mods))) = (gbm.as_ref(), buffers::pick_dmabuf_format(&c.dmabuf_formats)) {
                match gbm.alloc(w, h, *fourcc, mods) {
                    Ok(d) => {
                        let params = dmabuf.create_params(&self.qh, key.clone());
                        for (i, p) in d.planes.iter().enumerate() {
                            params.add(p.fd.as_fd(), i as u32, p.offset, p.stride, (d.modifier >> 32) as u32, d.modifier as u32);
                        }
                        let buffer = params.create_immed(
                            w as i32,
                            h as i32,
                            *fourcc,
                            zwp_linux_buffer_params_v1::Flags::empty(),
                            &self.qh,
                            busy.clone(),
                        );
                        params.destroy();
                        return Some(Slot { buffer, busy, width: w, height: h, stale: false });
                    }
                    Err(e) => log::warn!("dmabuf alloc {w}x{h} failed: {e}; falling back to shm"),
                }
            }
        }
        let format = [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888]
            .into_iter()
            .find(|f| c.shm_formats.contains(f))
            .or_else(|| c.shm_formats.first().copied())?;
        let stride = w * 4;
        let size = stride as usize * h as usize;
        let (fd, _) = buffers::shm_file(size, false).map_err(|e| log::error!("shm alloc: {e}")).ok()?;
        let pool = self.shm.create_pool(fd.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &self.qh, busy.clone());
        pool.destroy();
        Some(Slot { buffer, busy, width: w, height: h, stale: false })
    }

    fn with_capture(&mut self, key: &Key, f: impl FnOnce(&mut Overlay, usize)) {
        if let Some(ov) = self.overlay.as_mut() {
            if let Some(i) = ov.entries.iter().position(|e| &e.key == key && e.cap.is_some()) {
                f(ov, i);
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------------------------------

    fn maybe_commit_on_release(&mut self) {
        let commit = self.overlay.as_ref().is_some_and(|ov| !ov.sticky && ov.has_focus && !ov.holding());
        if commit {
            self.activate_selected();
        }
    }

    fn key_pressed(&mut self, k: u32) {
        let Some(ov) = self.overlay.as_mut() else { return };
        match k {
            key::ESC => self.hide(),
            key::TAB => {
                let d = if ov.shift() { -1 } else { 1 };
                ov.step(d);
            }
            key::ENTER | key::KPENTER | key::SPACE => self.activate_selected(),
            key::LEFT | key::H => ov.step(-1),
            key::RIGHT | key::L => ov.step(1),
            key::UP | key::K | key::DOWN | key::J => {
                let d = if matches!(k, key::UP | key::K) { -1 } else { 1 };
                if let Some(l) = &ov.layout {
                    let n = l.vertical_neighbor(ov.selected, d);
                    ov.select(n);
                }
            }
            key::HOME => ov.select(0),
            key::END => {
                let n = ov.entries.len().saturating_sub(1);
                ov.select(n);
            }
            key::W | key::Q | key::DELETE => self.close_selected(None),
            _ => {}
        }
    }

    fn pointer_moved(&mut self, x: f32, y: f32) {
        self.pointer_pos = Some((x, y));
        let Some(ov) = self.overlay.as_mut() else { return };
        let Some(l) = ov.layout.as_ref().filter(|_| ov.visible) else { return };
        if let Some(i) = l.hit(x, y) {
            let close = l.cells[i].close.offset(l.panel.x, l.panel.y).contains(x, y);
            ov.select(i);
            if close != ov.close_hover {
                ov.close_hover = close;
                ov.needs_render = true;
            }
        } else if ov.close_hover {
            ov.close_hover = false;
            ov.needs_render = true;
        }
    }

    fn pointer_button(&mut self, button: u32) {
        let Some((x, y)) = self.pointer_pos else { return };
        let Some(ov) = self.overlay.as_ref() else { return };
        let Some(l) = ov.layout.as_ref().filter(|_| ov.visible) else { return };
        let hit = l.hit(x, y);
        match (button, hit) {
            (BTN_LEFT, Some(i)) => {
                if l.cells[i].close.offset(l.panel.x, l.panel.y).contains(x, y) {
                    self.close_selected(Some(i));
                } else {
                    if let Some(ov) = self.overlay.as_mut() {
                        ov.selected = i;
                    }
                    self.activate_selected();
                }
            }
            (BTN_MIDDLE, Some(i)) => self.close_selected(Some(i)),
            (_, None) if !l.panel.contains(x, y) => self.hide(),
            _ => {}
        }
    }
}

/// Attach a captured buffer to its preview subsurface. COSMIC scales it via the viewport while
/// compositing, so the downscale happens in the compositor's GPU pass.
fn present(surface: WlSurface, viewport: &WpViewport, placed: Option<Rect>, cap: &mut Capture, slot: usize) {
    let Some(s) = cap.slots.get(slot).and_then(|s| s.as_ref()) else { return };
    if let Some(r) = placed {
        viewport.set_destination(r.w as i32, r.h as i32);
    }
    surface.attach(Some(&s.buffer), 0, 0);
    surface.damage_buffer(0, 0, s.width as i32, s.height as i32);
    surface.commit();
    s.busy.set(true);
}

fn destroy_entry(e: Entry) {
    // Surfaces first so no buffer is destroyed while attached.
    e.viewport.destroy();
    e.sub.destroy();
    e.surface.destroy();
    if let Some(c) = e.cap {
        c.destroy();
    }
}

// ----------------------------------------------------------------------------------------------
// Wayland dispatch
// ----------------------------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSubcompositor);
delegate_noop!(App: ignore WlSubsurface);
delegate_noop!(App: ignore WlRegion);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore WpFractionalScaleManagerV1);
delegate_noop!(App: ignore WpCursorShapeManagerV1);
delegate_noop!(App: ignore WpCursorShapeDeviceV1);
delegate_noop!(App: ignore ExtImageCopyCaptureManagerV1);
delegate_noop!(App: ignore ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(App: ignore ExtImageCaptureSourceV1);

impl Dispatch<ZwpLinuxDmabufV1, ()> for App {
    fn event(_: &mut Self, _: &ZwpLinuxDmabufV1, _: zwp_linux_dmabuf_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpLinuxBufferParamsV1, Key> for App {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            log::warn!("compositor rejected a dmabuf; switching that preview to shared memory");
            state.with_capture(key, |ov, i| {
                let cap = ov.entries[i].cap.as_mut().unwrap();
                cap.force_shm = true;
                cap.mark_all_stale();
            });
        }
    }
}

impl Dispatch<WlShm, ()> for App {
    fn event(state: &mut Self, _: &WlShm, event: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_shm::Event::Format { format: WEnum::Value(wl_shm::Format::Abgr8888) } = event {
            state.shm_abgr = true;
        }
    }
}

impl Dispatch<WlBuffer, BusyFlag> for App {
    fn event(_: &mut Self, _: &WlBuffer, event: wl_buffer::Event, busy: &BusyFlag, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            busy.set(false);
        }
    }
}

impl Dispatch<WlSurface, ()> for App {
    fn event(state: &mut Self, surface: &WlSurface, event: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            if let Some(ov) = state.overlay.as_mut().filter(|ov| &ov.root == surface) {
                if ov.int_scale != factor {
                    ov.int_scale = factor;
                    ov.needs_render = true;
                }
            }
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(state: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(ov) = state.overlay.as_mut() {
                if ov.scale120 != Some(scale) {
                    ov.scale120 = Some(scale);
                    ov.needs_render = true;
                }
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let Some(mut ov) = state.overlay.take() else { return };
                if &ov.layer == layer {
                    let size = (width.max(1) as i32, height.max(1) as i32);
                    if size != ov.size {
                        ov.size = size;
                        ov.needs_layout = true;
                    }
                    state.ensure_backdrop(&mut ov);
                    ov.root_viewport.set_destination(size.0, size.1);
                    ov.root.commit();
                    ov.configured = true;
                }
                state.overlay = Some(ov);
            }
            zwlr_layer_surface_v1::Event::Closed => state.hide(),
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                let p = seat.get_pointer(qh, ());
                state.cursor_device = state.cursor_shape_mgr.as_ref().map(|m| m.get_pointer(&p, qh, ()));
                state.pointer = Some(p);
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                if let Some(ov) = state.overlay.as_mut().filter(|ov| ov.root == surface) {
                    ov.has_focus = true;
                    ov.pressed = buffers::ne_u32s(&keys).collect();
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                if let Some(ov) = state.overlay.as_mut() {
                    ov.has_focus = false;
                    ov.pressed.clear();
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                if let Some(ov) = state.overlay.as_mut() {
                    ov.mods = mods_depressed;
                }
                state.maybe_commit_on_release();
            }
            wl_keyboard::Event::Key { key: k, state: WEnum::Value(ks), .. } => {
                let Some(ov) = state.overlay.as_mut() else { return };
                match ks {
                    wl_keyboard::KeyState::Pressed => {
                        ov.pressed.insert(k);
                        state.key_pressed(k);
                    }
                    wl_keyboard::KeyState::Released => {
                        ov.pressed.remove(&k);
                        state.maybe_commit_on_release();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for App {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                if let Some(d) = &state.cursor_device {
                    d.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                }
                // Don't select on enter: the pointer may simply be resting where the panel appeared.
                state.pointer_pos = Some((surface_x as f32, surface_y as f32));
            }
            wl_pointer::Event::Leave { .. } => state.pointer_pos = None,
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => state.pointer_moved(surface_x as f32, surface_y as f32),
            wl_pointer::Event::Button { button, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                state.pointer_button(button)
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                if let Some(ov) = state.overlay.as_mut() {
                    ov.scroll_acc += value;
                    while ov.scroll_acc.abs() >= 15.0 {
                        let d = ov.scroll_acc.signum();
                        ov.scroll_acc -= d * 15.0;
                        ov.step(d as isize);
                    }
                }
            }
            _ => {}
        }
    }
}

// --- Toplevel tracking ---------------------------------------------------------------------

impl Dispatch<ExtForeignToplevelListV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            let key = Key(toplevel.id());
            let cosmic = Some(state.cosmic_info.get_cosmic_toplevel(&toplevel, qh, key.clone()));
            state.toplevels.insert(
                key.clone(),
                Toplevel {
                    ext: toplevel,
                    cosmic,
                    title: String::new(),
                    app_id: String::new(),
                    activated: false,
                    minimized: false,
                    size: None,
                },
            );
            state.mru.push(key);
        }
    }

    event_created_child!(App, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let key = Key(handle.id());
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(t) = state.toplevels.get_mut(&key) {
                    t.title = title;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(t) = state.toplevels.get_mut(&key) {
                    t.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(ov) = state.overlay.as_mut() {
                    if ov.entries.iter().any(|e| e.key == key) {
                        ov.needs_render = true;
                    }
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.remove_entry(&key);
                state.mru.retain(|k| k != &key);
                if let Some(t) = state.toplevels.remove(&key) {
                    if let Some(c) = t.cosmic {
                        c.destroy();
                    }
                    t.ext.destroy();
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore ZcosmicToplevelManagerV1);

impl Dispatch<ZcosmicToplevelInfoV1, ()> for App {
    fn event(_: &mut Self, _: &ZcosmicToplevelInfoV1, _: zcosmic_toplevel_info_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

    // Only sent to v1 clients; declared so a stray event can't panic the dispatcher.
    event_created_child!(App, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, Key(ObjectId::null())),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, Key> for App {
    fn event(
        state: &mut Self,
        _: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zcosmic_toplevel_handle_v1::Event::State { state: raw } => {
                let mut activated = false;
                let mut minimized = false;
                for s in buffers::ne_u32s(&raw) {
                    match zcosmic_toplevel_handle_v1::State::try_from(s) {
                        Ok(zcosmic_toplevel_handle_v1::State::Activated) => activated = true,
                        Ok(zcosmic_toplevel_handle_v1::State::Minimized) => minimized = true,
                        _ => {}
                    }
                }
                let Some(t) = state.toplevels.get_mut(key) else { return };
                let newly_active = activated && !t.activated;
                t.activated = activated;
                t.minimized = minimized;
                if newly_active {
                    state.touch_mru(key);
                }
            }
            zcosmic_toplevel_handle_v1::Event::Geometry { width, height, .. } => {
                if let Some(t) = state.toplevels.get_mut(key) {
                    t.size = Some((width, height));
                }
            }
            _ => {}
        }
    }
}

// --- Capture --------------------------------------------------------------------------------

impl Dispatch<ExtImageCopyCaptureSessionV1, Key> for App {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event as E;
        state.with_capture(key, |ov, i| {
            let cap = ov.entries[i].cap.as_mut().unwrap();
            match event {
                E::BufferSize { width, height } => {
                    cap.incoming.width = width;
                    cap.incoming.height = height;
                }
                E::ShmFormat { format: WEnum::Value(f) } => cap.incoming.shm_formats.push(f),
                E::DmabufDevice { device } => cap.incoming.dmabuf_device = buffers::dev_t_from_bytes(&device),
                E::DmabufFormat { format, modifiers } => {
                    cap.incoming.dmabuf_formats.push((format, buffers::ne_u64s(&modifiers).collect()))
                }
                E::Done => {
                    let new = std::mem::take(&mut cap.incoming);
                    if cap.constraints.as_ref() != Some(&new) {
                        let resized = cap.constraints.as_ref().map(|c| (c.width, c.height)) != Some((new.width, new.height));
                        cap.mark_all_stale();
                        cap.constraints = Some(new);
                        if resized {
                            ov.needs_layout = true;
                        }
                    }
                }
                E::Stopped => {
                    cap.stopped = true;
                    ov.needs_render = true;
                }
                _ => {}
            }
        });
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, Key> for App {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event as E;
        if !matches!(event, E::Ready | E::Failed { .. }) {
            return;
        }
        let mut found = false;
        state.with_capture(key, |ov, i| {
            let visible = ov.visible;
            let e = &mut ov.entries[i];
            let cap = e.cap.as_mut().unwrap();
            let Some((f, slot)) = cap.frame.take_if(|(f, _)| f == frame) else { return };
            found = true;
            f.destroy();
            match event {
                E::Ready => {
                    cap.failures = 0;
                    let first = !cap.has_frame;
                    cap.has_frame = true;
                    if visible {
                        present(e.surface.clone(), &e.viewport, e.placed, cap, slot);
                    } else {
                        cap.pending = Some(slot);
                    }
                    if first {
                        ov.needs_render = true;
                    }
                }
                E::Failed { reason } => {
                    use ext_image_copy_capture_frame_v1::FailureReason as R;
                    match reason {
                        WEnum::Value(R::BufferConstraints) => cap.mark_all_stale(),
                        WEnum::Value(R::Stopped) => {
                            cap.stopped = true;
                            ov.needs_render = true;
                        }
                        _ => {
                            cap.failures += 1;
                            if cap.failures > 8 {
                                cap.stopped = true;
                                ov.needs_render = true;
                            } else {
                                cap.next_at = Some(Instant::now() + Duration::from_millis(100 * cap.failures as u64));
                            }
                        }
                    }
                }
                _ => {}
            }
        });
        if !found {
            frame.destroy();
        }
    }
}
