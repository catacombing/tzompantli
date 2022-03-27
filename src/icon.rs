use std::collections::HashMap;
use std::{fs, io};

use linicon::{IconPath, IconType};
use png::DecodingError;
use tiny_skia::{Pixmap, Transform};
use usvg::{FitTo, Options, Tree};
use xdg::BaseDirectories;

use crate::gl;

/// Desired size for PNG icons.
pub const ICON_SIZE: u32 = 64;

/// Icon texture cache.
#[derive(Debug)]
pub struct Icons {
    textures: HashMap<String, Texture>,
}

impl Icons {
    /// Load all installed applications.
    pub fn new() -> Self {
        let mut textures = HashMap::new();

        for name in XdgIcons::new().icons.drain(..) {
            let icon = match Icon::new(&name) {
                Some(icon) => icon,
                None => continue,
            };
            let texture = match icon.texture() {
                Ok(texture) => texture,
                Err(_) => continue,
            };
            textures.insert(name, texture);
        }

        Self { textures }
    }

    /// Retrieve textures for all loaded icons.
    pub fn textures(&self) -> impl Iterator<Item = &Texture> {
        self.textures.values()
    }

    /// Number of installed applications with icons.
    pub fn len(&self) -> usize {
        self.textures.len()
    }
}

/// Desktop entry icon path.
#[derive(Debug)]
struct Icon {
    path: IconPath,
}

impl From<IconPath> for Icon {
    fn from(path: IconPath) -> Self {
        Self { path }
    }
}

impl Icon {
    /// Lookup an icon path using its name.
    fn new(name: &str) -> Option<Self> {
        // Lookup all matching icons.
        let mut icons = linicon::lookup_icon(name)
            .with_size(ICON_SIZE as u16)
            .flat_map(|icon| icon.ok())
            .filter(|icon| icon.icon_type != IconType::XMP);

        // Short-circuit if first icon is already as SVG.
        let first = icons.next()?;
        if first.icon_type == IconType::SVG {
            return Some(first.into());
        }

        // Find SVG or return first PNG icon.
        Some(icons.find(|icon| icon.icon_type == IconType::SVG).unwrap_or(first).into())
    }

    /// Get a texture with the rendered icon.
    fn texture(&self) -> Result<Texture, Error> {
        let pixmap = self.pixmap()?;
        Ok(Texture::new(pixmap.data(), pixmap.width() as usize, pixmap.height() as usize))
    }

    /// Load image file as pixmap.
    fn pixmap(&self) -> Result<Pixmap, Error> {
        match self.path.icon_type {
            IconType::PNG => Ok(Pixmap::load_png(&self.path.path)?),
            IconType::SVG => {
                let mut options = Options::default();
                options.resources_dir = Some(self.path.path.clone());
                options.fontdb.load_system_fonts();

                let file = fs::read(&self.path.path)?;
                let tree = Tree::from_data(&file, &options.to_ref())?;

                let mut pixmap = Pixmap::new(ICON_SIZE, ICON_SIZE).ok_or(Error::InvalidSize)?;

                let size = FitTo::Size(ICON_SIZE, ICON_SIZE);
                resvg::render(&tree, size, Transform::default(), pixmap.as_mut())
                    .ok_or(Error::InvalidSize)?;

                Ok(pixmap)
            },
            IconType::XMP => unreachable!(),
        }
    }
}

/// OpenGL texture.
#[derive(Debug, Copy, Clone)]
pub struct Texture {
    pub id: u32,
    pub width: usize,
    pub height: usize,
}

impl Texture {
    /// Load a buffer as texture into OpenGL.
    fn new(buffer: &[u8], width: usize, height: usize) -> Self {
        assert!(buffer.len() >= width * height * 4);

        unsafe {
            let mut id = 0;
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR_MIPMAP_LINEAR as i32); // TODO: Required?
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR_MIPMAP_LINEAR as i32); // TODO: Required?
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
            gl::GenerateMipmap(gl::TEXTURE_2D); // TODO: Required?
            gl::BindTexture(gl::TEXTURE_2D, 0);
            Self { id, width, height }
        }
    }
}

/// Icon loading error.
#[derive(Debug)]
pub enum Error {
    PngDecodingError(DecodingError),
    SvgError(usvg::Error),
    IoError(io::Error),
    InvalidSize,
}

impl From<DecodingError> for Error {
    fn from(error: DecodingError) -> Self {
        Self::PngDecodingError(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::IoError(error)
    }
}

impl From<usvg::Error> for Error {
    fn from(error: usvg::Error) -> Self {
        Self::SvgError(error)
    }
}

struct XdgIcons {
    icons: Vec<String>,
}

impl XdgIcons {
    /// Get icons for all installed applications.
    fn new() -> Self {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new().expect("Unable to get XDG base directories");
        let dirs = base_dirs.get_data_dirs();

        // Find all desktop files in these directories, then look for their icons.
        let mut icons = Vec::new();
        for dir_entry in dirs.iter().map(|d| fs::read_dir(d.join("applications")).ok()).flatten() {
            for desktop_file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
                .flat_map(|entry| fs::read_to_string(entry.path()).ok())
            {
                if let Some(icon) = desktop_file.lines().find(|line| line.starts_with("Icon=")) {
                    icons.push(icon[5..].to_owned());
                }
            }
        }

        XdgIcons { icons }
    }
}
