//! The Wayland side: up to two surfaces on the wlr-layer-shell overlay layer (above windows, including a
//! fullscreen game on most compositors). Works on KDE Plasma, Hyprland, Sway, river, niri, Wayfire, COSMIC and
//! other compositors with layer-shell.
//!
//! - The **radar** is anchored to a screen corner with an empty input region, so every click goes through to
//!   the game.
//! - The **DPS meter** takes the pointer: its tabs, enemy name and reset button are clicked, and pressing
//!   anywhere else drags it. Layer surfaces cannot be moved by the compositor, so the meter moves itself: it is
//!   anchored to the top left corner and dragging changes its margins. Where it was left is saved.
//!
//!   The drag follows the mouse's own movement (the relative pointer protocol) rather than the pointer position
//!   on the surface, because compositors disagree about the latter while a moving surface holds the button:
//!   Sway keeps reporting it against where the surface was at the press, others against where it is now.
//!
//! GNOME has no layer-shell: there (or with `--window`) both open as small borderless windows. Drag them with
//! the left button; the right button opens the window menu, where "Always on Top" keeps them above the game.

use crate::meter_draw::{self, Hit};
use crate::overlay::Overlay;
use crate::radar::{BASE_H, BASE_W};
use crate::store::MeterSettings;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::ZwpRelativePointerV1;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer, BTN_LEFT, BTN_RIGHT},
        relative_pointer::{RelativeMotionEvent, RelativePointerHandler, RelativePointerState},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tiny_skia::Pixmap;

/// A press that moves less than this (logical pixels) before release is a click, not a drag.
const DRAG_THRESHOLD: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Corner {
    pub fn parse(s: &str) -> Option<Corner> {
        Some(match s {
            "top-left" => Corner::TopLeft,
            "top" => Corner::Top,
            "top-right" => Corner::TopRight,
            "right" => Corner::Right,
            "bottom-right" => Corner::BottomRight,
            "bottom" => Corner::Bottom,
            "bottom-left" => Corner::BottomLeft,
            "left" => Corner::Left,
            _ => return None,
        })
    }

    fn anchor(self) -> Anchor {
        match self {
            Corner::TopLeft => Anchor::TOP | Anchor::LEFT,
            Corner::Top => Anchor::TOP,
            Corner::TopRight => Anchor::TOP | Anchor::RIGHT,
            Corner::Right => Anchor::RIGHT,
            Corner::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
            Corner::Bottom => Anchor::BOTTOM,
            Corner::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
            Corner::Left => Anchor::LEFT,
        }
    }
}

pub struct MeterOptions {
    /// Top left corner on the screen, logical pixels.
    pub pos: (i32, i32),
    /// Size, 1 = the base layout; the mouse wheel over the meter changes it.
    pub scale: f32,
    /// Where the position and mode are saved.
    pub settings_path: PathBuf,
}

pub struct WindowOptions {
    pub corner: Corner,
    /// Distance from the screen edges in logical pixels: horizontal, vertical.
    pub margin: (i32, i32),
    pub scale: f32,
    /// Monitor name, as `wlr-randr` or the compositor lists it.
    pub output: Option<String>,
    /// Use normal windows even when layer-shell is there.
    pub force_window: bool,
    pub radar: bool,
    pub meter: Option<MeterOptions>,
}

/// Signals that flip the radar's visibility: SIGUSR1 hides or shows, SIGUSR2 pins the radar on.
pub struct Toggles {
    pub hide: Arc<AtomicBool>,
    pub always: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Radar,
    Meter,
}

enum Shell {
    Layer(LayerSurface),
    Window(Window),
}

impl Shell {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Shell::Layer(l) => l.wl_surface(),
            Shell::Window(w) => w.wl_surface(),
        }
    }
}

struct Panel {
    kind: Kind,
    shell: Shell,
    /// Logical size.
    width: u32,
    height: u32,
    buffer_scale: i32,
    configured: bool,
    /// Whether the last committed frame showed anything; a hidden radar is drawn once and then left alone.
    drawn_visible: Option<bool>,
    /// The meter's last input region height, so it is only sent when it changes.
    input_h: i32,
}

/// A left button press on the meter: a click if it stays put, a drag once it moves.
struct Press {
    panel: usize,
    /// Pointer position on the surface at the press.
    at: (f64, f64),
    /// The meter's place at the press.
    origin: (i32, i32),
    /// Mouse movement since the press, from the relative pointer.
    moved: (f64, f64),
    hit: Hit,
    dragging: bool,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    pool: SlotPool,
    conn: Connection,
    panels: Vec<Panel>,
    pointer: Option<ThemedPointer>,
    relative_state: RelativePointerState,
    relative: Option<ZwpRelativePointerV1>,
    seat: Option<wl_seat::WlSeat>,
    overlay: Overlay,
    meter_pos: (i32, i32),
    meter_scale: f32,
    settings_path: Option<PathBuf>,
    press: Option<Press>,
    exit: bool,
}

pub fn run(overlay: Overlay, options: WindowOptions, toggles: Toggles) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().map_err(|e| anyhow::anyhow!("cannot reach the Wayland compositor ({e}); is WAYLAND_DISPLAY set?"))?;
    let (globals, mut event_queue) = registry_queue_init::<App>(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("wl_compositor: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("wl_shm: {e}"))?;
    let layer_shell = if options.force_window { None } else { LayerShell::bind(&globals, &qh).ok() };
    let xdg_shell = XdgShell::bind(&globals, &qh).ok();
    if layer_shell.is_none() && xdg_shell.is_none() {
        anyhow::bail!("the compositor offers neither layer-shell nor xdg-shell");
    }

    let scale = options.scale;
    let radar_size = ((BASE_W * scale).round() as u32, (BASE_H * scale).round() as u32);
    let meter_scale = options.meter.as_ref().map_or(1.0, |m| m.scale);
    let meter_size = meter_size(meter_scale, overlay.meter_rows);
    let pool = SlotPool::new(((radar_size.0 * radar_size.1 + meter_size.0 * meter_size.1) * 4) as usize * 2, &shm)?;
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
        pool,
        conn: conn.clone(),
        panels: Vec::new(),
        pointer: None,
        relative_state: RelativePointerState::bind(&globals, &qh),
        relative: None,
        seat: None,
        overlay,
        meter_pos: options.meter.as_ref().map_or((0, 0), |m| (m.pos.0.max(0), m.pos.1.max(0))),
        meter_scale: options.meter.as_ref().map_or(1.0, |m| m.scale),
        settings_path: options.meter.as_ref().map(|m| m.settings_path.clone()),
        press: None,
        exit: false,
    };
    // Learn the outputs before choosing one.
    event_queue.roundtrip(&mut app)?;
    let output = options.output.as_ref().and_then(|name| {
        let found = app.output_state.outputs().find(|o| app.output_state.info(o).and_then(|i| i.name).as_deref() == Some(name.as_str()));
        if found.is_none() {
            let names: Vec<String> = app.output_state.outputs().filter_map(|o| app.output_state.info(&o).and_then(|i| i.name)).collect();
            log::warn!("no output named {name}; outputs: {}", names.join(", "));
        }
        found
    });

    let make = |kind: Kind, (w, h): (u32, u32), app: &mut App| -> anyhow::Result<()> {
        let surface = app.compositor.create_surface(&qh);
        let shell = match (&layer_shell, &xdg_shell) {
            (Some(layer_shell), _) => {
                let namespace = if kind == Kind::Radar { "aion-cube-overlay" } else { "aion-dps-meter" };
                let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(namespace), output.as_ref());
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.set_size(w, h);
                match kind {
                    Kind::Radar => {
                        layer.set_anchor(options.corner.anchor());
                        let (mx, my) = options.margin;
                        layer.set_margin(my, mx, my, mx);
                        // An empty input region: the pointer and clicks go to whatever is below, the game.
                        let region = Region::new(&app.compositor)?;
                        layer.wl_surface().set_input_region(Some(region.wl_region()));
                        log::info!("layer-shell radar, {w}x{h}, anchored {:?}", options.corner);
                    }
                    Kind::Meter => {
                        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
                        // Not pushed aside by panels: the margins are the position on the screen.
                        layer.set_exclusive_zone(-1);
                        let (x, y) = app.meter_pos;
                        layer.set_margin(y, 0, 0, x);
                        log::info!("layer-shell DPS meter, {w}x{h}, at {x},{y} (drag it to move it)");
                    }
                }
                layer.commit();
                Shell::Layer(layer)
            }
            (None, Some(xdg)) => {
                let window = xdg.create_window(surface, WindowDecorations::None, &qh);
                window.set_title(if kind == Kind::Radar { "Cube radar" } else { "DPS meter" });
                window.set_app_id(if kind == Kind::Radar { "aion-cube-overlay" } else { "aion-dps-meter" });
                window.set_min_size(Some((w, h)));
                window.set_max_size(Some((w, h)));
                window.commit();
                Shell::Window(window)
            }
            (None, None) => unreachable!(),
        };
        app.panels.push(Panel { kind, shell, width: w, height: h, buffer_scale: 1, configured: false, drawn_visible: None, input_h: -1 });
        Ok(())
    };
    if options.radar {
        make(Kind::Radar, radar_size, &mut app)?;
    }
    if options.meter.is_some() {
        make(Kind::Meter, meter_size, &mut app)?;
    }
    if layer_shell.is_none() {
        if options.force_window {
            log::info!("opened as normal windows (--window). Drag them with the left button; right click for the window menu.");
        } else {
            log::warn!("this compositor has no layer-shell (GNOME?): opened normal windows. Drag them with the left button; right click for the window menu and choose Always on Top.");
        }
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), event_queue).insert(event_loop.handle()).map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    let tick = Duration::from_millis(100);
    event_loop
        .handle()
        .insert_source(Timer::from_duration(tick), move |_, _, app: &mut App| {
            if toggles.hide.swap(false, Ordering::Relaxed) {
                app.overlay.toggle_hidden();
            }
            if toggles.always.swap(false, Ordering::Relaxed) {
                app.overlay.toggle_always();
            }
            app.overlay.step(crate::source::now_ms());
            app.draw_all();
            TimeoutAction::ToDuration(tick)
        })
        .map_err(|e| anyhow::anyhow!("timer: {e}"))?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

/// The meter's logical size at `scale`.
fn meter_size(scale: f32, rows: usize) -> (u32, u32) {
    ((meter_draw::W * scale).round() as u32, (meter_draw::height(rows) * scale).round() as u32)
}

impl App {
    /// Grows (positive `steps`) or shrinks the meter by 10 % a step, and saves the new size.
    fn resize_meter(&mut self, i: usize, steps: f64) {
        let scale = ((self.meter_scale as f64 * 1.1f64.powf(steps)) as f32).clamp(0.6, 3.0);
        let scale = (scale * 20.0).round() / 20.0;
        if scale == self.meter_scale {
            return;
        }
        self.meter_scale = scale;
        let (w, h) = meter_size(scale, self.overlay.meter_rows);
        match &self.panels[i].shell {
            Shell::Layer(layer) => {
                layer.set_size(w, h);
                layer.commit();
            }
            Shell::Window(window) => {
                window.set_min_size(Some((w, h)));
                window.set_max_size(Some((w, h)));
                window.commit();
            }
        }
        // The compositor's configure brings the new size; keep the meter on screen once it has it.
        log::info!("meter size {scale:.2}");
        self.save_meter();
    }

    fn panel_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.panels.iter().position(|p| p.shell.wl_surface() == surface)
    }

    fn draw_all(&mut self) {
        for i in 0..self.panels.len() {
            self.draw(i);
        }
    }

    fn draw(&mut self, i: usize) {
        let panel = &self.panels[i];
        if !panel.configured {
            return;
        }
        let visible = match panel.kind {
            Kind::Radar => self.overlay.visible(),
            Kind::Meter => true,
        };
        if !visible && panel.drawn_visible == Some(false) {
            return;
        }
        let bscale = panel.buffer_scale.max(1);
        let (pw, ph) = (panel.width * bscale as u32, panel.height * bscale as u32);
        let Some(mut pix) = Pixmap::new(pw, ph) else { return };
        let base_w = match panel.kind {
            Kind::Radar => BASE_W,
            Kind::Meter => meter_draw::W,
        };
        // Logical to physical, following the size the compositor gave the surface.
        let draw_scale = bscale as f32 * panel.width as f32 / base_w;
        match panel.kind {
            Kind::Radar => self.overlay.draw(&mut pix, draw_scale),
            Kind::Meter => self.overlay.draw_meter(&mut pix, draw_scale),
        }
        let (buffer, canvas) = match self.pool.create_buffer(pw as i32, ph as i32, pw as i32 * 4, wl_shm::Format::Argb8888) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("no shm buffer: {e}");
                return;
            }
        };
        // tiny-skia is premultiplied RGBA; ARGB8888 little endian is premultiplied BGRA in memory.
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pix.data().chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        let panel = &mut self.panels[i];
        let surface = panel.shell.wl_surface();
        if panel.kind == Kind::Meter {
            // Only the drawn panel takes the pointer; the empty rows below it let clicks through.
            let h = (self.overlay.meter_drawn_h * panel.width as f32 / meter_draw::W).ceil() as i32;
            if h != panel.input_h {
                if let Ok(region) = Region::new(&self.compositor) {
                    region.add(0, 0, panel.width as i32, h.min(panel.height as i32));
                    surface.set_input_region(Some(region.wl_region()));
                    panel.input_h = h;
                }
            }
        }
        surface.set_buffer_scale(bscale);
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        if let Err(e) = buffer.attach_to(surface) {
            log::warn!("could not attach the buffer: {e}");
            return;
        }
        surface.commit();
        panel.drawn_visible = Some(visible);
    }

    /// Logical pointer position on the meter to the meter's unscaled layout coordinates.
    fn meter_layout(&self, i: usize, pos: (f64, f64)) -> (f32, f32) {
        let k = meter_draw::W / self.panels[i].width as f32;
        (pos.0 as f32 * k, pos.1 as f32 * k)
    }

    /// Puts the meter at `(x, y)`, kept on the screen it is on.
    fn place_meter(&mut self, i: usize, x: i32, y: i32) {
        let (w, h) = (self.panels[i].width as i32, self.panels[i].height as i32);
        let screen = self.output_state.outputs().filter_map(|o| self.output_state.info(&o).and_then(|i| i.logical_size)).next();
        let (mut x, mut y) = (x, y);
        if let Some((sw, sh)) = screen {
            x = x.min(sw - w.min(sw));
            y = y.min(sh - (meter_draw::height(1) as i32).min(h));
        }
        x = x.max(0);
        y = y.max(0);
        if (x, y) == self.meter_pos {
            return;
        }
        self.meter_pos = (x, y);
        if let Shell::Layer(layer) = &self.panels[i].shell {
            layer.set_margin(y, 0, 0, x);
            layer.commit();
        }
    }

    /// Follows a drag: `(dx, dy)` is the movement since the press, or with `incremental` since the last move.
    fn drag_to(&mut self, dx: f64, dy: f64, incremental: bool) {
        let Some(press) = &mut self.press else { return };
        if !press.dragging {
            let total = if incremental { (dx, dy) } else { press.moved };
            if total.0.hypot(total.1) < DRAG_THRESHOLD {
                return;
            }
            press.dragging = true;
            self.set_cursor(CursorIcon::Grabbing);
        }
        let Some(press) = &self.press else { return };
        let (i, origin) = (press.panel, press.origin);
        let (x, y) = if incremental {
            (self.meter_pos.0 + dx.round() as i32, self.meter_pos.1 + dy.round() as i32)
        } else {
            (origin.0 + dx.round() as i32, origin.1 + dy.round() as i32)
        };
        self.place_meter(i, x, y);
    }

    fn save_meter(&self) {
        let Some(path) = &self.settings_path else { return };
        let settings = MeterSettings {
            x: self.meter_pos.0,
            y: self.meter_pos.1,
            mode: self.overlay.dps.mode.key().to_string(),
            scale: self.meter_scale,
            minimized: self.overlay.meter_minimized,
        };
        if let Err(e) = settings.save(path) {
            log::warn!("could not save the meter's place: {e}");
        }
    }

    fn set_cursor(&self, icon: CursorIcon) {
        if let Some(p) = &self.pointer {
            let _ = p.set_cursor(&self.conn, icon);
        }
    }

    fn click(&mut self, i: usize, hit: Hit) {
        if self.overlay.meter_click(hit) {
            self.save_meter();
        }
        self.draw(i);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, new_factor: i32) {
        if let Some(i) = self.panel_index(surface) {
            let panel = &mut self.panels[i];
            if new_factor != panel.buffer_scale {
                log::info!("output scale {new_factor}");
                panel.buffer_scale = new_factor.max(1);
                panel.drawn_visible = None;
                self.draw(i);
            }
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        log::info!("the compositor closed the overlay");
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(i) = self.panel_index(layer.wl_surface()) else { return };
        let panel = &mut self.panels[i];
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            panel.width = configure.new_size.0;
            panel.height = configure.new_size.1;
        }
        panel.configured = true;
        panel.drawn_visible = None;
        panel.input_h = -1;
        self.draw(i);
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, _: WindowConfigure, _: u32) {
        // The windows keep their own size: min and max size are both set to it.
        let Some(i) = self.panel_index(window.wl_surface()) else { return };
        if self.panels[i].kind == Kind::Meter {
            let (w, h) = meter_size(self.meter_scale, self.overlay.meter_rows);
            self.panels[i].width = w;
            self.panels[i].height = h;
        }
        self.panels[i].configured = true;
        self.panels[i].drawn_visible = None;
        self.panels[i].input_h = -1;
        self.draw(i);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            match self.seat_state.get_pointer_with_theme::<App, ()>(qh, &seat, self.shm.wl_shm(), cursor_surface, ThemeSpec::default()) {
                Ok(p) => {
                    self.relative = self.relative_state.get_relative_pointer(p.pointer(), qh).ok();
                    if self.relative.is_none() {
                        log::info!("no relative pointer protocol: dragging the meter follows the pointer position instead");
                    }
                    self.pointer = Some(p);
                    self.seat = Some(seat);
                }
                Err(e) => log::warn!("no pointer: {e}"),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(r) = self.relative.take() {
                r.destroy();
            }
            if let Some(p) = self.pointer.take() {
                p.pointer().release();
            }
            self.press = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            let Some(i) = self.panel_index(&event.surface) else { continue };
            let kind = self.panels[i].kind;
            let is_window = matches!(self.panels[i].shell, Shell::Window(_));
            match event.kind {
                PointerEventKind::Enter { .. } => self.set_cursor(CursorIcon::Default),
                // The wheel resizes the meter: up bigger, down smaller.
                PointerEventKind::Axis { vertical, .. } if kind == Kind::Meter => {
                    let steps = if vertical.value120 != 0 {
                        vertical.value120 as f64 / 120.0
                    } else if vertical.discrete != 0 {
                        vertical.discrete as f64
                    } else {
                        vertical.absolute / 15.0
                    };
                    if steps != 0.0 {
                        self.resize_meter(i, -steps);
                    }
                }
                PointerEventKind::Press { button, serial, .. } => {
                    let hit = if kind == Kind::Meter {
                        let (x, y) = self.meter_layout(i, event.position);
                        self.overlay.meter_hit(x, y)
                    } else {
                        Hit::Drag
                    };
                    if is_window {
                        // A normal window: the compositor moves it, and the controls act on the press.
                        let (Shell::Window(window), Some(seat)) = (&self.panels[i].shell, &self.seat) else { continue };
                        match button {
                            BTN_LEFT if hit == Hit::Drag => window.move_(seat, serial),
                            BTN_LEFT => self.click(i, hit),
                            BTN_RIGHT => window.show_window_menu(seat, serial, (event.position.0 as i32, event.position.1 as i32)),
                            _ => {}
                        }
                    } else if kind == Kind::Meter && button == BTN_LEFT {
                        self.press = Some(Press { panel: i, at: event.position, origin: self.meter_pos, moved: (0.0, 0.0), hit, dragging: false });
                    }
                }
                // Without the relative pointer protocol: the pointer position on the surface, assuming it is
                // reported against where the surface is now.
                PointerEventKind::Motion { .. } if kind == Kind::Meter && !is_window && self.relative.is_none() => {
                    let Some(press) = &self.press else { continue };
                    let (dx, dy) = (event.position.0 - press.at.0, event.position.1 - press.at.1);
                    self.drag_to(dx, dy, true);
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } if kind == Kind::Meter && !is_window => {
                    let Some(press) = self.press.take() else { continue };
                    if press.dragging {
                        self.set_cursor(CursorIcon::Default);
                        log::info!("meter moved to {},{}", self.meter_pos.0, self.meter_pos.1);
                        self.save_meter();
                    } else {
                        self.click(i, press.hit);
                    }
                }
                _ => {}
            }
        }
    }
}

impl RelativePointerHandler for App {
    fn relative_pointer_motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpRelativePointerV1, _: &wl_pointer::WlPointer, event: RelativeMotionEvent) {
        let Some(press) = &mut self.press else { return };
        press.moved.0 += event.delta.0;
        press.moved.1 += event.delta.1;
        let moved = press.moved;
        self.drag_to(moved.0, moved.1, false);
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);
