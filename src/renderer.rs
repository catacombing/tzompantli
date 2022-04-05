//! OpenGL rendering.

use std::ffi::CStr;
use std::{mem, ptr};

use crossfont::Size as FontSize;
use smithay::backend::egl::{self, EGLContext, EGLSurface};

use crate::apps::{App, Apps, ICON_SIZE};
use crate::gl::types::{GLfloat, GLint, GLuint};
use crate::{gl, Size};

/// Minimum padding between icons.
const MIN_PADDING: f32 = 64.;

/// Padding between icon and text.
const TEXT_PADDING: f32 = 16.;

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    uniform_position: GLint,
    uniform_matrix: GLint,
    size: Size<f32>,
    apps: Apps,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(
        font: &str,
        font_size: impl Into<FontSize>,
        context: &EGLContext,
        surface: &EGLSurface,
    ) -> Self {
        unsafe {
            // Setup OpenGL symbol loader.
            gl::load_with(|symbol| egl::get_proc_address(symbol));

            // Enable the OpenGL context.
            context.make_current_with_surface(surface).expect("Unable to enable OpenGL context");

            // Create vertex shader.
            let vertex_shader = gl::CreateShader(gl::VERTEX_SHADER);
            gl::ShaderSource(
                vertex_shader,
                1,
                [VERTEX_SHADER.as_ptr()].as_ptr() as *const _,
                &(VERTEX_SHADER.len() as i32) as *const _,
            );
            gl::CompileShader(vertex_shader);

            // Create fragment shader.
            let fragment_shader = gl::CreateShader(gl::FRAGMENT_SHADER);
            gl::ShaderSource(
                fragment_shader,
                1,
                [FRAGMENT_SHADER.as_ptr()].as_ptr() as *const _,
                &(FRAGMENT_SHADER.len() as i32) as *const _,
            );
            gl::CompileShader(fragment_shader);

            // Create shader program.
            let program = gl::CreateProgram();
            gl::AttachShader(program, vertex_shader);
            gl::AttachShader(program, fragment_shader);
            gl::LinkProgram(program);
            gl::UseProgram(program);

            // Generate VBO.
            let mut vbo = 0;
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            // Fill VBO with vertex positions.
            #[rustfmt::skip]
            let vertices: [GLfloat; 12] = [
                -1.0,  1.0, // Top-left
                -1.0, -1.0, // Bottom-left
                 1.0, -1.0, // Bottom-right

                -1.0,  1.0, // Top-left
                 1.0, -1.0, // Bottom-right
                 1.0,  1.0, // Top-right
            ];
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (mem::size_of::<GLfloat>() * vertices.len()) as isize,
                vertices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // Define VBO layout.
            let name = CStr::from_bytes_with_nul(b"aVertexPosition\0").unwrap();
            let location = gl::GetAttribLocation(program, name.as_ptr()) as GLuint;
            gl::VertexAttribPointer(
                location,
                2,
                gl::FLOAT,
                gl::FALSE,
                2 * mem::size_of::<GLfloat>() as i32,
                ptr::null(),
            );
            gl::EnableVertexAttribArray(0);

            // Set background color and blending.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

            // Get uniform locations.
            let name = CStr::from_bytes_with_nul(b"uPosition\0").unwrap();
            let uniform_position = gl::GetUniformLocation(program, name.as_ptr());
            let name = CStr::from_bytes_with_nul(b"uMatrix\0").unwrap();
            let uniform_matrix = gl::GetUniformLocation(program, name.as_ptr());

            // Load textures for all installed applications.
            let apps = Apps::new(font, font_size);

            Renderer { uniform_position, uniform_matrix, apps, size: Default::default() }
        }
    }

    /// Render all passed icon textures.
    pub fn draw(&self, offset: f32) {
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            let grid = self.grid_dimensions();
            for (i, app) in self.apps.iter().enumerate() {
                // Render the icon texture.
                let row = (i as f32 / grid.columns).floor();
                let icon_x = i as f32 % grid.columns * grid.width + grid.padding / 2.;
                let icon_y = row * grid.height + grid.padding / 2. + offset;
                let icon_size = Size::new(grid.icon_size, grid.icon_size);
                self.draw_texture_at(app.icon, icon_x, icon_y, icon_size);

                // Render the text texture.
                let text_x = icon_x + (grid.icon_size - app.text.width as f32) / 2.;
                let text_y = icon_y + grid.icon_size + TEXT_PADDING;
                self.draw_texture_at(app.text, text_x, text_y, None);
            }

            gl::Flush();
        }
    }

    /// Render texture at a position in viewport-coordinates.
    ///
    /// Specifying a `size` will automatically scale the texture to render at the desired size.
    /// Otherwise the texture's size will be used instead.
    unsafe fn draw_texture_at(
        &self,
        texture: Texture,
        mut x: f32,
        mut y: f32,
        size: impl Into<Option<Size<f32>>>,
    ) {
        let (width, height) = match size.into() {
            Some(Size { width, height }) => (width, height),
            None => (texture.width as f32, texture.height as f32),
        };

        // Matrix transforming vertex positions to desired size.
        let x_scale = width / self.size.width;
        let y_scale = height / self.size.height;
        let matrix = [x_scale, 0., 0., y_scale];
        gl::UniformMatrix2fv(self.uniform_matrix, 1, gl::FALSE, matrix.as_ptr());

        // Set texture position offset.
        x /= self.size.width / 2.;
        y /= self.size.height / 2.;
        gl::Uniform2fv(self.uniform_position, 1, [x, -y].as_ptr());

        gl::BindTexture(gl::TEXTURE_2D, texture.id);

        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size) {
        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();
    }

    /// Total unclipped height of all icons.
    pub fn content_height(&self) -> f32 {
        let grid = self.grid_dimensions();
        grid.rows * grid.height
    }

    /// App at the specified location.
    pub fn app_at(&self, position: (f64, f64)) -> Option<&App> {
        let grid = self.grid_dimensions();
        let column = (position.0 as f32 / grid.width).floor();
        let row = (position.1 as f32 / grid.height).floor();
        let index = (row * grid.columns + column) as usize;
        self.apps.iter().nth(index)
    }

    /// Compute grid dimensions.
    fn grid_dimensions(&self) -> GridDimensions {
        let icon_count = self.apps.len() as f32;
        let icon_size = ICON_SIZE as f32;

        let max_columns = (self.size.width / (icon_size + MIN_PADDING)).floor();
        let columns = max_columns.min(icon_count);
        let rows = (icon_count / columns).ceil();

        let padding = self.size.width / columns - icon_size;
        let width = icon_size + padding;
        let height = width;

        GridDimensions { icon_size, padding, columns, rows, width, height }
    }
}

/// Icon grid dimensions.
struct GridDimensions {
    icon_size: f32,
    padding: f32,
    columns: f32,
    rows: f32,
    width: f32,
    height: f32,
}

/// OpenGL texture.
#[derive(Debug, Copy, Clone)]
pub struct Texture {
    id: u32,
    pub width: usize,
    pub height: usize,
}

impl Texture {
    /// Load a buffer as texture into OpenGL.
    pub fn new(buffer: &[u8], width: usize, height: usize) -> Self {
        assert!(buffer.len() >= width * height * 4);

        unsafe {
            let mut id = 0;
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE as u32,
                buffer.as_ptr() as *const _,
            );
            gl::GenerateMipmap(gl::TEXTURE_2D);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            Self { id, width, height }
        }
    }
}
