use std::ffi::CStr;
use std::{mem, ptr};

use smithay::backend::egl::{self, EGLContext, EGLSurface};

use crate::gl::types::{GLfloat, GLint, GLuint};
use crate::icon::{Icons, ICON_SIZE};
use crate::{gl, Size};

/// Minimum padding between icons.
const MIN_PADDING: f32 = 16.;

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    uniform_position: GLint,
    uniform_matrix: GLint,
    size: Size<f32>,
    icons: Icons,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(context: &EGLContext, surface: &EGLSurface) -> Self {
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

            // Load all icon textures.
            let icons = Icons::new();

            Renderer { uniform_position, uniform_matrix, icons, size: Default::default() }
        }
    }

    /// Render all passed icon textures.
    pub fn draw(&self) {
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Compute icon grid size.
            let icon_size = ICON_SIZE as f32;
            let max_columns = (self.size.width / (icon_size + MIN_PADDING)).floor();
            let columns = max_columns.min(self.icons.len() as f32);
            let padding = self.size.width / columns - icon_size;
            let space_size = icon_size + padding;

            for (i, icon) in self.icons.textures().enumerate() {
                // TODO: Icons are blurry, likely be related to mipmaps/scaling.
                //
                // Matrix transforming vertex positions to desired icon size.
                let x_scale = icon_size / self.size.width / 2.;
                let y_scale = icon_size / self.size.height / 2.;
                let matrix = [x_scale, 0., 0., y_scale];
                gl::UniformMatrix2fv(self.uniform_matrix, 1, gl::FALSE, matrix.as_ptr());

                // Set icon position offset.
                let mut x_position = i as f32 % columns * space_size + padding / 2.;
                let mut y_position = (i as f32 / columns).floor() * space_size + padding / 2.;
                x_position /= self.size.width / 2.;
                y_position /= self.size.height / 2.;
                gl::Uniform2fv(self.uniform_position, 1, [x_position, -y_position].as_ptr());

                gl::BindTexture(gl::TEXTURE_2D, icon.id);

                gl::DrawArrays(gl::TRIANGLES, 0, 6);
            }

            gl::Flush();
        }
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size) {
        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();
    }
}
