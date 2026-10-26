//! The Wayland side: a wlr-layer-shell surface on the overlay layer (above windows, including a fullscreen game
//! on most compositors), anchored to a screen corner, with an empty input region so every click goes through to
//! the game. Works on KDE Plasma, Hyprland, Sway, river, niri, Wayfire, COSMIC and other compositors with
//! layer-shell.
//!
//! GNOME has no layer-shell: there (or with `--window`) the radar opens as a small borderless window instead.
//! Drag it with the left button; the right button opens the window menu, where "Always on Top" keeps it above
//! the game.

use crate::overlay::Overlay;
use crate::radar::{BASE_H, BASE_W};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT, BTN_RIGHT},
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tiny_skia::Pixmap;

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

pub struct WindowOptions {
    pub corner: Corner,
    /// Distance from the screen edges in logical pixels: horizontal, vertical.
    pub margin: (i32, i32),
    pub scale: f32,
    /// Monitor name, as `wlr-randr` or the compositor lists it.
    pub output: Option<String>,
    /// Use a normal window even when layer-shell is there.
    pub force_window: bool,
}

/// Signals that flip the visibility: SIGUSR1 hides or shows, SIGUSR2 pins the radar on.
pub struct Toggles {
    pub hide: Arc<AtomicBool>,
    pub always: Arc<AtomicBool>,
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

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    shell: Option<Shell>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    overlay: Overlay,
    /// Logical size.
    width: u32,
    height: u32,
    user_scale: f32,
    buffer_scale: i32,
    configured: bool,
    /// Whether the last committed frame showed the radar; a hidden overlay is drawn once and then left alone.
    drawn_visible: Option<bool>,
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

    let width = (BASE_W * options.scale).round() as u32;
    let height = (BASE_H * options.scale).round() as u32;
    let pool = SlotPool::new((width * height * 4) as usize * 2, &shm)?;
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        shell: None,
        pointer: None,
        seat: None,
        overlay,
        width,
        height,
        user_scale: options.scale,
        buffer_scale: 1,
        configured: false,
        drawn_visible: None,
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

    let surface = compositor.create_surface(&qh);
    match (layer_shell, xdg_shell) {
        (Some(layer_shell), _) => {
            let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("aion-cube-overlay"), output.as_ref());
            layer.set_anchor(options.corner.anchor());
            let (mx, my) = options.margin;
            layer.set_margin(my, mx, my, mx);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_size(width, height);
            // An empty input region: the pointer and clicks go to whatever is below, the game.
            let region = Region::new(&compositor)?;
            layer.wl_surface().set_input_region(Some(region.wl_region()));
            layer.commit();
            log::info!("layer-shell overlay, {width}x{height}, anchored {:?}", options.corner);
            app.shell = Some(Shell::Layer(layer));
        }
        (None, Some(xdg)) => {
            let window = xdg.create_window(surface, WindowDecorations::None, &qh);
            window.set_title("Cube radar");
            window.set_app_id("aion-cube-overlay");
            window.set_min_size(Some((width, height)));
            window.set_max_size(Some((width, height)));
            window.commit();
            if options.force_window {
                log::info!("opened as a normal window (--window). Drag it with the left button; right click for the window menu.");
            } else {
                log::warn!("this compositor has no layer-shell (GNOME?): opened a normal window. Drag it with the left button; right click for the window menu and choose Always on Top.");
            }
            app.shell = Some(Shell::Window(window));
        }
        (None, None) => anyhow::bail!("the compositor offers neither layer-shell nor xdg-shell"),
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
            app.draw();
            TimeoutAction::ToDuration(tick)
        })
        .map_err(|e| anyhow::anyhow!("timer: {e}"))?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

impl App {
    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let visible = self.overlay.visible();
        if !visible && self.drawn_visible == Some(false) {
            return;
        }
        let Some(shell) = &self.shell else { return };
        let scale = self.buffer_scale.max(1);
        let (pw, ph) = (self.width * scale as u32, self.height * scale as u32);
        let Some(mut pix) = Pixmap::new(pw, ph) else { return };
        self.overlay.draw(&mut pix, self.user_scale * scale as f32 * self.width as f32 / (BASE_W * self.user_scale).round());
        let stride = pw as i32 * 4;
        let (buffer, canvas) = match self.pool.create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888) {
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
        let surface = shell.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        if let Err(e) = buffer.attach_to(surface) {
            log::warn!("could not attach the buffer: {e}");
            return;
        }
        surface.commit();
        self.drawn_visible = Some(visible);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, new_factor: i32) {
        if new_factor != self.buffer_scale {
            log::info!("output scale {new_factor}");
            self.buffer_scale = new_factor.max(1);
            self.drawn_visible = None;
            self.draw();
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

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        self.configured = true;
        self.drawn_visible = None;
        self.draw();
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, _: WindowConfigure, _: u32) {
        // The window keeps its own size: min and max size are both set to it.
        self.configured = true;
        self.drawn_visible = None;
        self.draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        // Only the fallback window takes the pointer (to be moved); the layer surface lets every click through.
        if capability == Capability::Pointer && self.pointer.is_none() && matches!(self.shell, Some(Shell::Window(_)) | None) {
            if let Ok(p) = self.seat_state.get_pointer(qh, &seat) {
                self.pointer = Some(p);
                self.seat = Some(seat);
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let (Some(Shell::Window(window)), Some(seat)) = (&self.shell, &self.seat) else { return };
        for event in events {
            if &event.surface != window.wl_surface() {
                continue;
            }
            if let PointerEventKind::Press { button, serial, .. } = event.kind {
                match button {
                    BTN_LEFT => window.move_(seat, serial),
                    BTN_RIGHT => window.show_window_menu(seat, serial, (event.position.0 as i32, event.position.1 as i32)),
                    _ => {}
                }
            }
        }
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
