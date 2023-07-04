use std::mem;
use std::num::NonZeroU32;
use std::ops::{Div, Mul};

use glutin::api::egl::config::Config;
use glutin::api::egl::context::PossiblyCurrentContext;
use glutin::api::egl::display::Display;
use glutin::api::egl::surface::Surface;
use glutin::config::{Api, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::{self, GlobalList};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_touch, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::protocols::fractional_scale::{FractionalScaleHandler, FractionalScaleManager};
use crate::protocols::viewporter::Viewporter;
use crate::renderer::Renderer;

mod dbus;
mod protocols;
mod renderer;
mod svg;
mod text;
mod xdg;

mod gl {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

/// Maximum distance before a tap is considered a tap.
const MAX_TAP_DELTA: f64 = 20.;

/// Default font.
const FONT: &str = "Sans";

/// Default font size.
const FONT_SIZE: f32 = 6.;

/// Speed multiplier when using pointer rather than touch scrolling.
const POINTER_SPEED: f64 = 10.;

fn main() {
    // Initialize Wayland connection.
    let connection = Connection::connect_to_env().expect("Unable to find Wayland socket");
    let (globals, mut queue) =
        globals::registry_queue_init(&connection).expect("initialize event queue");

    let mut state = State::new(&connection, &globals, &mut queue);

    // Start event loop.
    while !state.terminated {
        queue.blocking_dispatch(&mut state).unwrap();
    }
}

/// Wayland protocol handler state.
#[derive(Debug)]
pub struct State {
    protocol_states: ProtocolStates,
    touch_start: (f64, f64),
    frame_pending: bool,
    last_touch_y: f64,
    terminated: bool,
    is_tap: bool,
    offset: f64,
    factor: f64,
    size: Size,

    egl_context: Option<PossiblyCurrentContext>,
    egl_surface: Option<Surface<WindowSurface>>,
    egl_config: Option<Config>,
    viewport: Option<WpViewport>,
    renderer: Option<Renderer>,
    window: Option<Window>,
    touch: Option<WlTouch>,
    pointer: Option<WlPointer>,
}

impl State {
    fn new(connection: &Connection, globals: &GlobalList, queue: &mut EventQueue<Self>) -> Self {
        // Setup globals.
        let queue_handle = queue.handle();
        let protocol_states = ProtocolStates::new(globals, &queue_handle);

        // Default to a desktop-like initial size, if the compositor asks for 0×0 it
        // actually means we are free to pick whichever size we want.
        let size = Size { width: 640, height: 480 };

        let mut state = Self {
            factor: 1.,
            protocol_states,
            size,
            frame_pending: Default::default(),
            last_touch_y: Default::default(),
            touch_start: Default::default(),
            egl_context: Default::default(),
            egl_surface: Default::default(),
            egl_config: Default::default(),
            terminated: Default::default(),
            viewport: Default::default(),
            renderer: Default::default(),
            is_tap: Default::default(),
            offset: Default::default(),
            window: Default::default(),
            touch: Default::default(),
            pointer: Default::default(),
        };

        state.init_window(connection, &queue_handle);

        state
    }

    /// Initialize the window and its EGL surface.
    fn init_window(&mut self, connection: &Connection, queue: &QueueHandle<Self>) {
        // Initialize EGL context.
        let mut raw_display = WaylandDisplayHandle::empty();
        raw_display.display = connection.backend().display_ptr().cast();
        let raw_display = RawDisplayHandle::Wayland(raw_display);
        let display = unsafe { Display::new(raw_display).expect("Unable to create EGL display") };

        let config_template = ConfigTemplateBuilder::new().with_api(Api::GLES2).build();
        let config = unsafe {
            display
                .find_configs(config_template)
                .ok()
                .and_then(|mut configs| configs.next())
                .expect("No suitable configuration found")
        };

        let context_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        let context = unsafe {
            display
                .create_context(&config, &context_attributes)
                .expect("Failed to create EGL context")
        };

        // Create the Wayland surface.
        let surface = self.protocol_states.compositor.create_surface(queue);

        // Initialize fractional scaling protocol.
        self.protocol_states.fractional_scale.fractional_scaling(queue, &surface);

        // Initialize viewporter protocol.
        let viewport = self.protocol_states.viewporter.viewport(queue, &surface);

        let context = context.treat_as_possibly_current();

        // Create the window.
        let decorations = WindowDecorations::RequestServer;
        let window = self.protocol_states.xdg_shell.create_window(surface, decorations, queue);
        window.set_title("Tzompantli");
        window.set_app_id("Tzompantli");
        window.commit();

        self.egl_context = Some(context);
        self.egl_config = Some(config);
        self.viewport = Some(viewport);
        self.window = Some(window);
    }

    /// Render the application state.
    fn draw(&mut self) {
        let offset = self.offset as f32;
        self.renderer().draw(offset);
        self.frame_pending = false;

        if let Err(error) = self.egl_surface().swap_buffers(self.egl_context()) {
            eprintln!("Buffer swap failed: {error:?}");
        }
    }

    fn resize(&mut self, size: Size) {
        let scale_factor = self.factor;
        self.size = size;

        if self.egl_surface.is_none() {
            // Create the EGL surface.
            let mut raw_window_handle = WaylandWindowHandle::empty();
            raw_window_handle.surface = self.window().wl_surface().id().as_ptr().cast();
            let raw_window_handle = RawWindowHandle::Wayland(raw_window_handle);
            let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
                raw_window_handle,
                NonZeroU32::new(self.size.width as u32).unwrap(),
                NonZeroU32::new(self.size.height as u32).unwrap(),
            );

            let config = self.egl_config.as_ref().expect("EGL config access before initialization");
            let egl_surface = unsafe {
                self.display()
                    .create_window_surface(config, &surface_attributes)
                    .expect("Failed to create EGL surface")
            };
            self.egl_surface = Some(egl_surface);
        }

        // Update opaque region.
        let logical_size = size / self.factor;
        if let Ok(region) = Region::new(&self.protocol_states.compositor) {
            region.add(0, 0, logical_size.width, logical_size.height);
            self.window().wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        // Set viewporter DST size.
        if let Some(viewport) = &self.viewport {
            viewport.set_destination(logical_size.width, logical_size.height);
        }

        self.egl_surface().resize(
            self.egl_context(),
            NonZeroU32::new(size.width as u32).unwrap(),
            NonZeroU32::new(size.height as u32).unwrap(),
        );
        self.renderer().resize(size, scale_factor);
        self.draw();
    }

    fn renderer(&mut self) -> &mut Renderer {
        // Initialize renderer on demand.
        //
        // This is necessary because with the OpenGL context current, the EGL surface
        // cannot be resized without swapping buffers at least once.
        if self.renderer.is_none() {
            let _ = self.egl_context().make_current(self.egl_surface());
            self.renderer = Some(Renderer::new(FONT, FONT_SIZE, &self.display()));
        }

        unsafe { self.renderer.as_mut().unwrap_unchecked() }
    }

    fn egl_surface(&self) -> &Surface<WindowSurface> {
        self.egl_surface.as_ref().expect("EGL surface access before initialization")
    }

    fn egl_context(&self) -> &PossiblyCurrentContext {
        self.egl_context.as_ref().expect("EGL context access before initialization")
    }

    fn display(&self) -> Display {
        self.egl_context().display()
    }

    fn window(&self) -> &Window {
        self.window.as_ref().expect("Window access before initialization")
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![OutputState, SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _factor: i32,
    ) {
        // NOTE: We exclusively use fractional scaling.
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
        self.draw();
    }
}

impl FractionalScaleHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        factor: f64,
    ) {
        let factor_change = factor / self.factor;
        self.factor = factor;

        if self.egl_surface.is_some() {
            self.resize(self.size * factor_change);
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.protocol_states.output
    }

    fn new_output(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}

impl WindowHandler for State {
    fn request_close(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _window: &Window,
    ) {
        self.terminated = true;
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        // Use current size to trigger initial draw if no dimensions were provided.
        let size = configure.new_size.0.zip(configure.new_size.1);
        let size = size
            .map(|size| Size::from((size.0.get(), size.1.get())) * self.factor)
            .unwrap_or(self.size);
        self.resize(size);
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.protocol_states.seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Touch && self.touch.is_none() {
            self.touch = self.protocol_states.seat.get_touch(queue, &seat).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.protocol_states.seat.get_pointer(queue, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Touch {
            if let Some(touch) = self.touch.take() {
                touch.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl TouchHandler for State {
    fn down(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _surface: WlSurface,
        _id: i32,
        position: (f64, f64),
    ) {
        // Scale touch position by scale factor.
        let position = (position.0 * self.factor, position.1 * self.factor);

        self.last_touch_y = position.1;
        self.touch_start = position;
        self.is_tap = true;
    }

    fn up(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _id: i32,
    ) {
        // Ignore drags.
        if !self.is_tap {
            return;
        }

        // Start application at touch point and exit.
        let mut position = self.touch_start;
        position.1 -= self.offset;
        if let Err(err) = self.renderer().exec_at(position) {
            eprintln!("Could not launch application: {err}");
        }
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        _id: i32,
        position: (f64, f64),
    ) {
        // Scale touch position by scale factor.
        let position = (position.0 * self.factor, position.1 * self.factor);

        // Calculate delta since touch start.
        let delta = (self.touch_start.0 - position.0, self.touch_start.1 - position.1);

        // Ignore drag until maximum tap distance is exceeded.
        if self.is_tap && f64::sqrt(delta.0.powi(2) + delta.1.powi(2)) <= MAX_TAP_DELTA {
            return;
        }
        self.is_tap = false;

        // Compute new offset.
        let last_y = mem::replace(&mut self.last_touch_y, position.1);
        self.offset += self.last_touch_y - last_y;

        // Clamp offset to content size.
        let max = -self.renderer().content_height() as f64 + self.size.height as f64;
        self.offset = self.offset.clamp(max.min(0.), 0.);

        // Request a new frame, if there is no pending frame already.
        if !self.frame_pending {
            self.frame_pending = true;

            let surface = self.window().wl_surface();
            surface.frame(queue, surface.clone());
            surface.commit();
        }
    }

    fn cancel(&mut self, _connection: &Connection, _queue: &QueueHandle<Self>, _touch: &WlTouch) {}

    fn shape(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _major: f64,
        _minor: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _orientation: f64,
    ) {
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                PointerEventKind::Press { .. } => {
                    // Start application at pointer position and exit.
                    let mut position = event.position;
                    position.1 -= self.offset;
                    if let Err(err) = self.renderer().exec_at(position) {
                        eprintln!("Could not launch application: {err}");
                    }
                },
                PointerEventKind::Axis { vertical: AxisScroll { absolute, .. }, .. } => {
                    self.offset += absolute * POINTER_SPEED * self.factor;

                    // Clamp offset to content size.
                    let max = -self.renderer().content_height() as f64 + self.size.height as f64;
                    self.offset = self.offset.clamp(max.min(0.), 0.);

                    // Request a new frame, if there is no pending frame already.
                    if !self.frame_pending {
                        self.frame_pending = true;

                        let surface = self.window().wl_surface();
                        surface.frame(queue, surface.clone());
                        surface.commit();
                    }
                },
                PointerEventKind::Enter { .. }
                | PointerEventKind::Leave { .. }
                | PointerEventKind::Motion { .. }
                | PointerEventKind::Release { .. } => (),
            }
        }
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_seat!(State);
delegate_touch!(State);
delegate_pointer!(State);

delegate_registry!(State);

#[derive(Debug)]
struct ProtocolStates {
    fractional_scale: FractionalScaleManager,
    compositor: CompositorState,
    registry: RegistryState,
    viewporter: Viewporter,
    xdg_shell: XdgShell,
    output: OutputState,
    seat: SeatState,
}

impl ProtocolStates {
    fn new(globals: &GlobalList, queue: &QueueHandle<State>) -> Self {
        Self {
            registry: RegistryState::new(globals),
            fractional_scale: FractionalScaleManager::new(globals, queue)
                .expect("missing wp_fractional_scale"),
            compositor: CompositorState::bind(globals, queue).expect("missing wl_compositor"),
            viewporter: Viewporter::new(globals, queue).expect("missing wp_viewporter"),
            xdg_shell: XdgShell::bind(globals, queue).expect("missing xdg_shell"),
            output: OutputState::new(globals, queue),
            seat: SeatState::new(globals, queue),
        }
    }
}

#[derive(Copy, Clone, Default, Debug)]
pub struct Size<T = i32> {
    pub width: T,
    pub height: T,
}

impl From<(u32, u32)> for Size {
    fn from(tuple: (u32, u32)) -> Self {
        Self { width: tuple.0 as i32, height: tuple.1 as i32 }
    }
}

impl From<Size> for Size<f32> {
    fn from(from: Size) -> Self {
        Self { width: from.width as f32, height: from.height as f32 }
    }
}

impl Mul<f64> for Size {
    type Output = Self;

    fn mul(mut self, factor: f64) -> Self {
        self.width = (self.width as f64 * factor) as i32;
        self.height = (self.height as f64 * factor) as i32;
        self
    }
}

impl Div<f64> for Size {
    type Output = Self;

    fn div(mut self, factor: f64) -> Self {
        self.width = (self.width as f64 / factor).round() as i32;
        self.height = (self.height as f64 / factor).round() as i32;
        self
    }
}
