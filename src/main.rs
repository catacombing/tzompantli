use smithay::backend::egl::context::GlAttributes;
use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::native::{EGLNativeDisplay, EGLPlatform};
use smithay::backend::egl::{ffi, EGLContext, EGLSurface};
use smithay::egl_platform;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_display::WlDisplay;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_registry::WlRegistry;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{
    Connection, ConnectionHandle, EventQueue, Proxy, QueueHandle,
};
use smithay_client_toolkit::reexports::protocols::xdg_shell::client::xdg_surface::XdgSurface;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowHandler, XdgWindowState};
use smithay_client_toolkit::shell::xdg::{XdgShellHandler, XdgShellState};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_registry, delegate_xdg_shell,
    delegate_xdg_window,
};
use wayland_egl::WlEglSurface;

use crate::renderer::Renderer;

mod icon;
mod renderer;

mod gl {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

/// Attributes for OpenGL context creation.
const GL_ATTRIBUTES: GlAttributes =
    GlAttributes { version: (2, 0), profile: None, debug: false, vsync: false };

fn main() {
    // Initialize Wayland connection.
    let connection = Connection::connect_to_env().expect("Unable to find Wayland socket");
    let mut queue = connection.new_event_queue();

    let mut state = State::new(&connection, &mut queue);

    // Start event loop.
    while !state.terminated {
        queue.blocking_dispatch(&mut state).unwrap();
    }
}

/// Wayland protocol handler state.
#[derive(Debug)]
struct State {
    protocol_states: ProtocolStates,
    terminated: bool,
    size: Size,

    egl_context: Option<EGLContext>,
    egl_surface: Option<EGLSurface>,
    renderer: Option<Renderer>,
    window: Option<Window>,
}

impl State {
    fn new(connection: &Connection, queue: &mut EventQueue<Self>) -> Self {
        // Setup globals.
        let mut connection_handle = connection.handle();
        let display = connection_handle.display();
        let registry = display
            .get_registry(&mut connection_handle, &queue.handle(), ())
            .expect("Unable to create registry");
        let protocol_states = ProtocolStates::new(registry);

        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        let mut state = Self {
            protocol_states,
            size,
            egl_context: Default::default(),
            egl_surface: Default::default(),
            terminated: Default::default(),
            renderer: Default::default(),
            window: Default::default(),
        };

        // Manually drop connection handle to prevent deadlock during dispatch.
        drop(connection_handle);

        // Roundtrip to initialize globals.
        queue.blocking_dispatch(&mut state).unwrap();
        queue.blocking_dispatch(&mut state).unwrap();

        state.init_window(&mut connection.handle(), &queue.handle());

        state
    }

    /// Initialize the window and its EGL surface.
    fn init_window(&mut self, connection: &mut ConnectionHandle, queue: &QueueHandle<Self>) {
        // Initialize EGL context.
        let native_display = NativeDisplay::new(connection.display());
        let display = EGLDisplay::new(&native_display, None).expect("Unable to create EGL display");
        let context =
            EGLContext::new_with_config(&display, GL_ATTRIBUTES, Default::default(), None)
                .expect("Unable to create EGL context");

        // Create the Wayland surface.
        let surface = self
            .protocol_states
            .compositor
            .create_surface(connection, &queue)
            .expect("Unable to create surface");

        // Create the EGL surface.
        let config = context.config_id();
        let native_surface = WlEglSurface::new(surface.id(), self.size.width, self.size.height)
            .expect("Unable to create EGL surface");
        let pixel_format = context.pixel_format().expect("No valid pixel format present");
        let egl_surface = EGLSurface::new(&display, pixel_format, config, native_surface, None)
            .expect("Unable to bind EGL surface");

        // Create the window.
        let window = self
            .protocol_states
            .xdg_window
            .create_window(connection, &queue, surface)
            .expect("Unable to create window");
        window.set_title(connection, "Tzompantli");
        window.set_app_id(connection, "Tzompantli");
        window.map(connection, &queue);

        // Initialize the renderer.
        let renderer = Renderer::new(&context, &egl_surface);

        self.egl_surface = Some(egl_surface);
        self.egl_context = Some(context);
        self.renderer = Some(renderer);
        self.window = Some(window);
    }

    /// Render the application state.
    fn draw(&mut self, connection: &mut ConnectionHandle, queue: &QueueHandle<Self>) {
        self.renderer().draw();

        // Request a new frame. Commit is done by `swap_buffers`.
        let surface = self.window().wl_surface();
        surface.frame(connection, queue, surface.clone()).expect("create callback");

        if let Err(error) = self.egl_surface().swap_buffers(None) {
            eprintln!("Buffer swap failed: {:?}", error);
        }
    }

    fn egl_surface(&self) -> &EGLSurface {
        self.egl_surface.as_ref().expect("EGL surface access before initialization")
    }

    fn renderer(&mut self) -> &mut Renderer {
        self.renderer.as_mut().expect("Renderer access before initialization")
    }

    fn window(&self) -> &Window {
        self.window.as_ref().expect("Window access before initialization")
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.protocol_states.compositor
    }

    fn scale_factor_changed(
        &mut self,
        _connection: &mut ConnectionHandle,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _factor: i32,
    ) {
    }

    fn frame(
        &mut self,
        connection: &mut ConnectionHandle,
        queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
        self.draw(connection, queue);
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.protocol_states.output
    }

    fn new_output(
        &mut self,
        _connection: &mut ConnectionHandle,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _connection: &mut ConnectionHandle,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _connection: &mut ConnectionHandle,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.protocol_states.xdg_shell
    }

    fn configure(
        &mut self,
        connection: &mut ConnectionHandle,
        queue: &QueueHandle<Self>,
        _surface: &XdgSurface,
    ) {
        if let Some(new_size) = self.window().configure().and_then(|configure| configure.new_size) {
            self.size = new_size.into();
            let size = self.size;
            self.egl_surface().resize(size.width, size.height, 0, 0);
            self.renderer().resize(size);
            self.draw(connection, queue);
        }
    }
}

impl WindowHandler for State {
    fn xdg_window_state(&mut self) -> &mut XdgWindowState {
        &mut self.protocol_states.xdg_window
    }

    fn request_close_window(
        &mut self,
        _connection: &mut ConnectionHandle,
        _queue: &QueueHandle<Self>,
        _window: &Window,
    ) {
        self.terminated = true;
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);

delegate_registry!(State: [
    CompositorState,
    OutputState,
    XdgShellState,
    XdgWindowState,
]);

#[derive(Debug)]
struct ProtocolStates {
    compositor: CompositorState,
    xdg_window: XdgWindowState,
    xdg_shell: XdgShellState,
    registry: RegistryState,
    output: OutputState,
}

impl ProtocolStates {
    fn new(registry: WlRegistry) -> Self {
        Self {
            registry: RegistryState::new(registry),
            compositor: CompositorState::new(),
            xdg_window: XdgWindowState::new(),
            xdg_shell: XdgShellState::new(),
            output: OutputState::new(),
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

struct NativeDisplay {
    display: WlDisplay,
}

impl NativeDisplay {
    fn new(display: WlDisplay) -> Self {
        Self { display }
    }
}

impl EGLNativeDisplay for NativeDisplay {
    fn supported_platforms(&self) -> Vec<EGLPlatform<'_>> {
        let display = self.display.id().as_ptr();
        vec![
            egl_platform!(PLATFORM_WAYLAND_KHR, display, &["EGL_KHR_platform_wayland"]),
            egl_platform!(PLATFORM_WAYLAND_EXT, display, &["EGL_EXT_platform_wayland"]),
        ]
    }
}
