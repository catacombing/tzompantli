//! OpenGL rendering.

use std::ffi::CStr;
use std::{cmp, mem, ptr};

use crossfont::Size as FontSize;
use smithay::backend::egl::{self, EGLContext, EGLSurface};

use crate::gl::types::{GLfloat, GLint, GLuint};
use crate::text::Rasterizer;
use crate::xdg::{DesktopEntries, DesktopEntry, ICON_SIZE};
use crate::{gl, Size};

/// Minimum horizontal padding between apps.
const MIN_PADDING_X: usize = 64;

/// Additional vertical padding between apps.
const PADDING_Y: usize = 16;

/// Padding between icon and text.
const TEXT_PADDING: usize = 16;

/// Maximum number of icon rows in one texture.
const MAX_TEXTURE_ROWS: usize = 5;

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    uniform_position: GLint,
    uniform_matrix: GLint,
    size: Size<f32>,
    entries: DesktopEntries,
    rasterizer: Rasterizer,
    textures: Vec<Texture>,
    grid: Grid,
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

            // Create the text rasterizer.
            let rasterizer = Rasterizer::new(font, font_size)
                .expect("Unable to create FreeType font rasterizer");

            // Lookup available applications.
            let entries = DesktopEntries::new();

            Renderer {
                uniform_position,
                uniform_matrix,
                rasterizer,
                entries,
                textures: Default::default(),
                size: Default::default(),
                grid: Default::default(),
            }
        }
    }

    /// Update the textures for the application grid.
    fn update_textures(&mut self) {
        // Ignore sizes where no icon fits on the screen.
        let width = self.size.width as usize;
        if width < ICON_SIZE as usize + MIN_PADDING_X {
            return;
        }

        self.textures.clear();

        self.grid = Grid::new(width, self.rasterizer.line_height(), self.entries.len());
        let max_width = self.grid.entry_width;
        let row_size = width * 4;

        let mut entries = self.entries.iter();

        let mut rows_remaining = self.grid.rows;
        while rows_remaining > 0 {
            let rows = cmp::min(rows_remaining, MAX_TEXTURE_ROWS);
            let buffer_size = rows * self.grid.entry_height * row_size;
            let mut buffer = TextureBuffer::new(buffer_size, row_size);

            for (spot, entry) in self.grid.zip(&mut entries) {
                buffer.write_rgba_at(&entry.icon.data, entry.icon.width * 4, spot.icon);
                let _ = self.rasterizer.rasterize(&mut buffer, spot.text, &entry.name, max_width);
            }

            let height = buffer.inner.len() / (width * 4);
            self.textures.push(Texture::new(&buffer.inner, width, height));

            rows_remaining = rows_remaining.saturating_sub(MAX_TEXTURE_ROWS);
            self.grid.index = 0;
        }
    }

    /// Render all passed icon textures.
    pub fn draw(&self, mut offset: f32) {
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Render all textures.
            for texture in &self.textures {
                // Skip textures above the viewport.
                offset += texture.height as f32;
                if offset < 0. {
                    continue;
                }

                // Render visible textures.
                self.draw_texture_at(*texture, 0., offset - texture.height as f32, None);

                // Skip textures below the viewport.
                if offset > self.size.height as f32 {
                    break;
                }
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
        self.update_textures();
    }

    /// Total unclipped height of all icons.
    pub fn content_height(&self) -> f32 {
        self.textures.iter().map(|texture| texture.height as f32).sum()
    }

    /// App at the specified location.
    pub fn app_at(&self, position: (f64, f64)) -> Option<&DesktopEntry> {
        self.entries.get(self.grid.index_at(position))
    }
}

/// Icon grid.
#[derive(Debug, Default, Copy, Clone)]
struct Grid {
    index: usize,

    columns: usize,
    rows: usize,

    icon_size: usize,
    entry_width: usize,
    entry_height: usize,
    padding_x: usize,
    padding_y: usize,
}

impl Grid {
    fn new(width: usize, line_height: usize, icon_count: usize) -> Self {
        let icon_size = ICON_SIZE as usize;

        let max_columns = width / (icon_size + MIN_PADDING_X);
        let columns = max_columns.min(icon_count).max(1);
        let rows = (icon_count as f32 / columns as f32).ceil() as usize;

        let padding_x = (width / columns - icon_size) / 2;
        let padding_y = TEXT_PADDING + PADDING_Y;
        let entry_width = icon_size + padding_x * 2;
        let entry_height = icon_size + line_height + padding_y * 2;

        Self { index: 0, columns, rows, icon_size, entry_width, entry_height, padding_x, padding_y }
    }

    /// Index of application at the specified location.
    fn index_at(&self, position: (f64, f64)) -> usize {
        let col = position.0 as usize / self.entry_width;
        let row = position.1 as usize / self.entry_height;
        row * self.columns + col
    }
}

impl Iterator for Grid {
    type Item = GridEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let col = self.index % self.columns;
        let row = self.index / self.columns;

        // Stop iterator once we've reached the maximum number of rows.
        if row >= MAX_TEXTURE_ROWS {
            self.index = 0;
            return None;
        }

        let icon_x = col * self.entry_width + self.padding_x;
        let icon_y = row * self.entry_height + self.padding_y;

        let text_x = icon_x + self.icon_size / 2;
        let text_y = icon_y + self.icon_size + TEXT_PADDING;

        self.index += 1;

        Some(GridEntry { icon: (icon_x, icon_y), text: (text_x, text_y) })
    }
}

/// One space inside the grid.
#[derive(Debug)]
struct GridEntry {
    /// Top-left corner for the icon texture.
    icon: (usize, usize),
    /// Top-center for the text texture.
    text: (usize, usize),
}

/// Helper for building the output texture.
pub struct TextureBuffer {
    inner: Vec<u8>,
    width: usize,
}

impl TextureBuffer {
    fn new(size: usize, width: usize) -> Self {
        Self { inner: vec![0; size], width }
    }

    /// Write an RGBA buffer at the specified location.
    pub fn write_rgba_at(&mut self, buffer: &[u8], width: usize, pos: (usize, usize)) {
        for row in 0..buffer.len() / width {
            let dst_start = (pos.1 + row) * self.width + pos.0 * 4;
            if dst_start >= self.inner.len() {
                break;
            }
            let dst_row = &mut self.inner[dst_start..];

            let src_start = row * width;
            let src_row = &buffer[src_start..src_start + cmp::min(width, dst_row.len())];

            let pixels = src_row.chunks(4).enumerate().filter(|(_i, pixel)| pixel != &[0, 0, 0, 0]);
            for (i, pixel) in pixels {
                dst_row[i * 4..i * 4 + 4].copy_from_slice(&pixel)
            }
        }
    }

    /// Write an RGB buffer at the specified location.
    pub fn write_rgb_at(&mut self, buffer: &[u8], width: usize, pos: (usize, usize)) {
        for row in 0..buffer.len() / width {
            let dst_start = (pos.1 + row) * self.width + pos.0 * 4;
            if dst_start >= self.inner.len() {
                break;
            }
            let dst_row = &mut self.inner[dst_start..];

            let src_start = row * width;
            let src_row = &buffer[src_start..src_start + cmp::min(width, dst_row.len() / 4 * 3)];

            let pixels = src_row.chunks(3).enumerate().filter(|(_i, pixel)| pixel != &[0, 0, 0]);
            for (i, pixel) in pixels {
                dst_row[i * 4..i * 4 + 3].copy_from_slice(&pixel);
            }
        }
    }
}

/// OpenGL texture.
#[derive(Debug, Copy, Clone)]
pub struct Texture {
    id: u32,
    pub width: usize,
    pub height: usize,
}

impl Default for Texture {
    fn default() -> Self {
        Texture::new(&[], 0, 0)
    }
}

impl Texture {
    /// Load a buffer as texture into OpenGL.
    pub fn new(buffer: &[u8], width: usize, height: usize) -> Self {
        assert!(buffer.len() == width * height * 4);

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
