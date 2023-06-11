//! OpenGL rendering.

use std::error::Error;
use std::ffi::{CStr, CString};
use std::process::{self, Command};
use std::{cmp, mem, ptr};

use crossfont::Size as FontSize;
use glutin::api::egl::display::Display;
use glutin::display::GlDisplay;

use crate::gl::types::{GLfloat, GLint, GLuint};
use crate::svg::{self, Svg};
use crate::text::Rasterizer;
use crate::xdg::DesktopEntries;
use crate::{dbus, gl, Size};

/// Minimum horizontal padding between apps.
const MIN_PADDING_X: usize = 64;

/// Additional vertical padding between apps.
const PADDING_Y: usize = 16;

/// Padding between icon and text.
const TEXT_PADDING: usize = 16;

/// Maximum number of icon rows in one texture.
const MAX_TEXTURE_ROWS: usize = 1;

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    power_menu: PowerMenu,
    entries: DesktopEntries,

    uniform_position: GLint,
    uniform_matrix: GLint,

    rasterizer: Rasterizer,
    textures: Vec<Texture>,
    size: Size<f32>,
    grid: Grid,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(font: &str, font_size: impl Into<FontSize>, display: &Display) -> Self {
        unsafe {
            // Setup OpenGL symbol loader.
            gl::load_with(|symbol| {
                let symbol = CString::new(symbol).unwrap();
                display.get_proc_address(symbol.as_c_str()).cast()
            });

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
            let rasterizer = Rasterizer::new(font, font_size, 1.)
                .expect("Unable to create FreeType font rasterizer");

            // Lookup available applications.
            let entries = DesktopEntries::new(1.).expect("Unable to load desktop entries");

            // Load power menu SVGs.
            let power_menu =
                PowerMenu::new(entries.icon_size()).expect("Unable to rasterize power SVGs");

            Renderer {
                uniform_position,
                uniform_matrix,
                rasterizer,
                power_menu,
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
        if width < self.entries.icon_size() as usize + MIN_PADDING_X {
            return;
        }

        self.textures.clear();

        self.grid = Grid::new(&self.entries, width, self.rasterizer.line_height());
        let max_width = self.grid.entry_width;
        let row_size = width * 4;

        // Update power menu icon textures.

        // Create texture buffer for this row.
        let buffer_size = self.grid.entry_height * row_size;
        let mut buffer = TextureBuffer::new(buffer_size, row_size);

        // Write power button to texture.
        let poweroff_spot = self.grid.spot(0);
        let svg = &self.power_menu.poweroff;
        buffer.write_rgba_at(&svg.data, svg.width * 4, poweroff_spot.icon);
        let _ = self.rasterizer.rasterize(&mut buffer, poweroff_spot.text, "Poweroff", max_width);

        // Write reboot button to texture.
        let reboot_spot = self.grid.spot(self.grid.columns.saturating_sub(1));
        let svg = &self.power_menu.reboot;
        buffer.write_rgba_at(&svg.data, svg.width * 4, reboot_spot.icon);
        let _ = self.rasterizer.rasterize(&mut buffer, reboot_spot.text, "Reboot", max_width);

        // Stage texture buffer for rendering.
        let height = buffer.inner.len() / (width * 4);
        self.textures.push(Texture::new(&buffer.inner, width, height));

        // Update app icon textures.

        // Create first icon texture buffer.
        let mut buffer = TextureBuffer::new(buffer_size, row_size);

        for (i, entry) in self.entries.iter().enumerate() {
            // Swap to next texture when this one is full.
            let texture_index = i % self.grid.columns * MAX_TEXTURE_ROWS;
            if i != 0 && texture_index == 0 {
                // Stage existing texture.
                let height = buffer.inner.len() / (width * 4);
                self.textures.push(Texture::new(&buffer.inner, width, height));

                // Create new texture buffer.
                buffer = TextureBuffer::new(buffer_size, row_size);
            }

            // Write icon data to the texture.
            let spot = self.grid.spot(texture_index);
            let _ = self.rasterizer.rasterize(&mut buffer, spot.text, &entry.name, max_width);
            buffer.write_rgba_at(&entry.icon.data, entry.icon.width * 4, spot.icon);
        }

        // Stage the last icon texture buffer.
        let height = buffer.inner.len() / (width * 4);
        self.textures.push(Texture::new(&buffer.inner, width, height));
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
                if offset > self.size.height {
                    break;
                }
            }

            gl::Flush();
        }
    }

    /// Render texture at a position in viewport-coordinates.
    ///
    /// Specifying a `size` will automatically scale the texture to render at
    /// the desired size. Otherwise the texture's size will be used instead.
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
    pub fn resize(&mut self, size: Size, scale_factor: f64) {
        // Update DPR.
        let _ = self.entries.set_scale_factor(scale_factor);
        self.rasterizer.set_scale_factor(scale_factor);
        self.power_menu.resize(self.entries.icon_size());

        // Resize textures.
        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();
        self.update_textures();
    }

    /// Total unclipped height of all icons.
    pub fn content_height(&self) -> f32 {
        self.textures.iter().map(|texture| texture.height as f32).sum()
    }

    /// Execute application at the specified location.
    pub fn exec_at(&self, position: (f64, f64)) -> Result<(), Box<dyn Error>> {
        let mut index = self.grid.index_at(position);

        // Check if click was on power menu row or on the app grid.
        if index == 0 {
            dbus::shutdown()
        } else if index == self.grid.columns.saturating_sub(1) {
            dbus::reboot()
        } else {
            // Get executable from grid.
            index -= self.grid.columns;
            let exec = self.entries.get(index).map(|entry| entry.exec.as_str());

            // Launch as a new process.
            if let Some(exec) = exec {
                let cmd = exec.split(' ').collect::<Vec<_>>();
                Command::new(cmd[0]).args(&cmd[1..]).spawn()?;
                process::exit(0);
            }

            Ok(())
        }
    }
}

/// Power menu icons.
#[derive(Debug)]
struct PowerMenu {
    poweroff: Svg,
    reboot: Svg,
    size: u32,
}

impl PowerMenu {
    /// Rasterize power menu icons.
    fn new(size: u32) -> Result<Self, svg::Error> {
        const POWEROFF_SVG: &[u8] = include_bytes!("../svgs/poweroff.svg");
        const REBOOT_SVG: &[u8] = include_bytes!("../svgs/reboot.svg");

        let poweroff = Svg::from_buffer(POWEROFF_SVG, size)?;
        let reboot = Svg::from_buffer(REBOOT_SVG, size)?;

        Ok(Self { poweroff, reboot, size })
    }

    /// Resize the power menu icons.
    fn resize(&mut self, size: u32) {
        // Ignore no-ops to prevent re-rasterization.
        if self.size == size {
            return;
        }

        // Attempt to re-rasterize at new size.
        if let Ok(power_menu) = Self::new(size) {
            *self = power_menu;
        }
    }
}

/// Icon grid.
#[derive(Debug, Default, Copy, Clone)]
struct Grid {
    columns: usize,

    icon_size: usize,
    entry_width: usize,
    entry_height: usize,
    padding_x: usize,
    padding_y: usize,
}

impl Grid {
    fn new(entries: &DesktopEntries, width: usize, line_height: usize) -> Self {
        let icon_size = entries.icon_size() as usize;
        let icon_count = entries.len().max(2);

        let max_columns = width / (icon_size + MIN_PADDING_X);
        let columns = max_columns.min(icon_count).max(1);

        let padding_x = (width / columns - icon_size) / 2;
        let padding_y = TEXT_PADDING + PADDING_Y;
        let entry_width = icon_size + padding_x * 2;
        let entry_height = icon_size + line_height + padding_y * 2;

        Self { columns, icon_size, entry_width, entry_height, padding_x, padding_y }
    }

    /// Position of the index in the grid.
    fn spot(&self, index: usize) -> GridEntry {
        let col = index % self.columns;
        let row = index / self.columns;

        let icon_x = col * self.entry_width + self.padding_x;
        let icon_y = row * self.entry_height + self.padding_y;

        let text_x = icon_x + self.icon_size / 2;
        let text_y = icon_y + self.icon_size + TEXT_PADDING;

        GridEntry { icon: (icon_x as isize, icon_y as isize), text: (text_x, text_y) }
    }

    /// Index of application at the specified location.
    fn index_at(&self, position: (f64, f64)) -> usize {
        let col = position.0 as usize / self.entry_width;
        let row = position.1 as usize / self.entry_height;
        row * self.columns + col
    }
}

/// One space inside the grid.
#[derive(Debug)]
struct GridEntry {
    /// Top-left corner for the icon texture.
    icon: (isize, isize),
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
    pub fn write_rgba_at(&mut self, buffer: &[u8], width: usize, pos: (isize, isize)) {
        self.write_buffer_inner::<4>(buffer, width, pos)
    }

    /// Write an RGB buffer at the specified location.
    pub fn write_rgb_at(&mut self, buffer: &[u8], width: usize, pos: (isize, isize)) {
        self.write_buffer_inner::<3>(buffer, width, pos);
    }

    fn write_buffer_inner<const N: usize>(
        &mut self,
        buffer: &[u8],
        mut width: usize,
        pos: (isize, isize),
    ) {
        // Clamp dst to zero.
        let dst_x = pos.0.max(0);

        // Compute pixels to cut off at the beginning of each row.
        let dst_x_offset = (dst_x - pos.0).unsigned_abs();
        width -= dst_x_offset * N;

        for row in 0..buffer.len() / width {
            let dst_start = (pos.1 + row as isize) * self.width as isize + dst_x * 4;

            // Skip rows outside the destination buffer.
            if dst_start < 0 {
                continue;
            }

            if dst_start >= self.inner.len() as isize {
                break;
            }

            let dst_row = &mut self.inner[dst_start as usize..];

            // Compute the start with-in the buffer.
            let src_start = row * width + dst_x_offset * N;
            let src_row = &buffer[src_start..src_start + cmp::min(width, dst_row.len() / 4 * N)];

            let pixels = src_row.chunks(N).enumerate().filter(|(_i, pixel)| pixel != &[0; N]);
            for (i, pixel) in pixels {
                dst_row[i * 4..i * 4 + N].copy_from_slice(pixel);
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
                gl::UNSIGNED_BYTE,
                buffer.as_ptr() as *const _,
            );
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            Self { id, width, height }
        }
    }
}
