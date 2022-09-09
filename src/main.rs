use std::mem;
use std::num::NonZeroU32;
use std::ops::Mul;
use std::process::{self, Command};

use glutin::api::egl::context::PossiblyCurrentContext;
use glutin::api::egl::display::Display;
use glutin::api::egl::surface::Surface;
use glutin::config::{Api, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowHandler, XdgWindowState,
};
use smithay_client_toolkit::shell::xdg::{XdgShellHandler, XdgShellState};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_registry, delegate_seat, delegate_touch,
    delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::renderer::Renderer;

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

fn main() {
    // Initialize Wayland connection.
    let mut connection = Connection::connect_to_env().expect("Unable to find Wayland socket");
    let mut queue = connection.new_event_queue();

    let mut state = State::new(&mut connection, &mut queue);

    // Start event loop.
    while !state.terminated {
        queue.blocking_dispatch(&mut state).unwrap();
    }
}

/// Wayland protocol handler state.
#[derive(Debug)]
struct State {
    protocol_states: ProtocolStates,
    touch_start: (f64, f64),
    frame_pending: bool,
    last_touch_y: f64,
    terminated: bool,
    is_tap: bool,
    offset: f64,
    factor: i32,
    size: Size,

    egl_context: Option<PossiblyCurrentContext>,
    egl_surface: Option<Surface<WindowSurface>>,
    renderer: Option<Renderer>,
    window: Option<Window>,
    touch: Option<WlTouch>,
}

impl State {
    fn new(connection: &mut Connection, queue: &mut EventQueue<Self>) -> Self {
        // Setup globals.
        let queue_handle = queue.handle();
        let protocol_states = ProtocolStates::new(connection, &queue_handle);

        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        let mut state = Self {
            factor: 1,
            protocol_states,
            size,
            frame_pending: Default::default(),
            last_touch_y: Default::default(),
            touch_start: Default::default(),
            egl_context: Default::default(),
            egl_surface: Default::default(),
            terminated: Default::default(),
            renderer: Default::default(),
            is_tap: Default::default(),
            offset: Default::default(),
            window: Default::default(),
            touch: Default::default(),
        };

        // Roundtrip to initialize globals.
        queue.blocking_dispatch(&mut state).unwrap();
        queue.blocking_dispatch(&mut state).unwrap();

        state.init_window(connection, &queue_handle);

        state
    }

    /// Initialize the window and its EGL surface.
    fn init_window(&mut self, connection: &mut Connection, queue: &QueueHandle<Self>) {
        // Initialize EGL context.
        let mut raw_display = WaylandDisplayHandle::empty();
        raw_display.display = connection.backend().display_ptr().cast();
        let raw_display = RawDisplayHandle::Wayland(raw_display);

        let display =
            unsafe { Display::from_raw(raw_display).expect("Unable to create EGL display") };

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
        let surface = self
            .protocol_states
            .compositor
            .create_surface(queue)
            .expect("Unable to create surface");

        // Create the EGL surface.
        let mut raw_window_handle = WaylandWindowHandle::empty();
        raw_window_handle.surface = surface.id().as_ptr().cast();
        let raw_window_handle = RawWindowHandle::Wayland(raw_window_handle);
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(self.size.width as u32).unwrap(),
            NonZeroU32::new(self.size.height as u32).unwrap(),
        );

        let egl_surface = unsafe {
            display
                .create_window_surface(&config, &surface_attributes)
                .expect("Failed to create EGL surface")
        };

        let context = context.make_current(&egl_surface).expect("Failed to make context current");

        // Create the window.
        let window = Window::builder()
            .title("Tzompantli")
            .app_id("Tzompantli")
            .map(
                queue,
                &self.protocol_states.xdg_shell,
                &mut self.protocol_states.xdg_window,
                surface,
            )
            .expect("Unable to create window");

        // Initialize the renderer.
        let renderer = Renderer::new(FONT, FONT_SIZE, &context);

        self.egl_surface = Some(egl_surface);
        self.egl_context = Some(context);
        self.renderer = Some(renderer);
        self.window = Some(window);
    }

    /// Render the application state.
    fn draw(&mut self) {
        let offset = self.offset as f32;
        self.renderer().draw(offset);
        self.frame_pending = false;

        if let Err(error) = self.egl_surface().swap_buffers(self.egl_context()) {
            eprintln!("Buffer swap failed: {:?}", error);
        }
    }

    fn resize(&mut self, size: Size) {
        let scale_factor = self.factor;
        self.size = size;

        self.egl_surface().resize(
            self.egl_context(),
            NonZeroU32::new(size.width as u32).unwrap(),
            NonZeroU32::new(size.height as u32).unwrap(),
        );
        self.renderer().resize(size, scale_factor);
        self.draw();
    }

    fn egl_surface(&self) -> &Surface<WindowSurface> {
        self.egl_surface.as_ref().expect("EGL surface access before initialization")
    }

    fn egl_context(&self) -> &PossiblyCurrentContext {
        self.egl_context.as_ref().expect("EGL context access before initialization")
    }

    fn renderer(&mut self) -> &mut Renderer {
        self.renderer.as_mut().expect("Renderer access before initialization")
    }

    fn window(&self) -> &Window {
        self.window.as_ref().expect("Window access before initialization")
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![CompositorState, OutputState, XdgShellState, XdgWindowState, SeatState,];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.protocol_states.compositor
    }

    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        factor: i32,
    ) {
        self.window().wl_surface().set_buffer_scale(factor);

        let factor_change = factor as f64 / self.factor as f64;
        self.factor = factor;

        self.resize(self.size * factor_change);
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

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.protocol_states.xdg_shell
    }
}

impl WindowHandler for State {
    fn xdg_window_state(&mut self) -> &mut XdgWindowState {
        &mut self.protocol_states.xdg_window
    }

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
        let size = configure
            .new_size
            .map(|size| Size::mul(size.into(), self.factor as f64))
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
        let position = (position.0 * self.factor as f64, position.1 * self.factor as f64);

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
        if let Some(exec) = self.renderer().exec_at(position) {
            let cmd = exec.split(' ').collect::<Vec<_>>();
            Command::new(cmd[0]).args(&cmd[1..]).spawn().unwrap();
            process::exit(0);
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
        let position = (position.0 * self.factor as f64, position.1 * self.factor as f64);

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
        self.offset = self.offset.min(0.).max(max.min(0.));

        // Request a new frame, if there is no pending frame already.
        if !self.frame_pending {
            self.frame_pending = true;

            let surface = self.window().wl_surface();
            surface.frame(queue, surface.clone()).expect("create callback");
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

delegate_compositor!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_seat!(State);
delegate_touch!(State);

delegate_registry!(State);

#[derive(Debug)]
struct ProtocolStates {
    compositor: CompositorState,
    xdg_window: XdgWindowState,
    xdg_shell: XdgShellState,
    registry: RegistryState,
    output: OutputState,
    seat: SeatState,
}

impl ProtocolStates {
    fn new(connection: &Connection, queue: &QueueHandle<State>) -> Self {
        Self {
            registry: RegistryState::new(connection, queue),
            compositor: CompositorState::new(),
            xdg_window: XdgWindowState::new(),
            xdg_shell: XdgShellState::new(),
            output: OutputState::new(),
            seat: SeatState::new(),
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
