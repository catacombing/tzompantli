use std::{fs, io};

use linicon::{IconPath, IconType};
use png::DecodingError;
use tiny_skia::{Pixmap, Transform};
use usvg::{FitTo, Options, Tree};
use xdg::BaseDirectories;

use crate::gl;

/// Desired size for PNG icons.
pub const ICON_SIZE: u32 = 64;

/// List of installed applications.
#[derive(Debug)]
pub struct Apps {
    apps: Vec<(DesktopEntry, Texture)>,
}

impl Apps {
    /// Load all installed applications.
    pub fn new() -> Self {
        // Get a list of all applications sorted by name.
        let mut entries = DesktopEntries::new().entries;
        entries.sort_unstable();
        entries.dedup();

        let mut apps = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            let icon = match Icon::new(&entry.name) {
                Some(icon) => icon,
                None => continue,
            };
            let texture = match icon.texture() {
                Ok(texture) => texture,
                Err(_) => continue,
            };
            apps.push((entry, texture));
        }

        Self { apps }
    }

    /// Iterate over all installed applications.
    pub fn iter(&self) -> impl Iterator<Item = &(DesktopEntry, Texture)> {
        self.apps.iter()
    }

    /// Number of installed applications with icons.
    pub fn len(&self) -> usize {
        self.apps.len()
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
                let resources_dir = Some(self.path.path.clone());
                let mut options = Options { resources_dir, ..Options::default() };
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

/// Icon loading error.
#[derive(Debug)]
pub enum Error {
    PngDecoding(DecodingError),
    Svg(usvg::Error),
    Io(io::Error),
    InvalidSize,
}

impl From<DecodingError> for Error {
    fn from(error: DecodingError) -> Self {
        Self::PngDecoding(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<usvg::Error> for Error {
    fn from(error: usvg::Error) -> Self {
        Self::Svg(error)
    }
}

struct DesktopEntries {
    entries: Vec<DesktopEntry>,
}

impl DesktopEntries {
    /// Get icons for all installed applications.
    fn new() -> Self {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new().expect("Unable to get XDG base directories");
        let dirs = base_dirs.get_data_dirs();

        // Find all desktop files in these directories, then look for their icons and executables.
        let mut icons = Vec::new();
        for dir_entry in dirs.iter().flat_map(|d| fs::read_dir(d.join("applications")).ok()) {
            for desktop_file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
                .flat_map(|entry| fs::read_to_string(entry.path()).ok())
            {
                let mut name = None;
                let mut exec = None;
                for line in desktop_file.lines() {
                    if let Some(value) = line.strip_prefix("Icon=") {
                        name = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("Exec=") {
                        exec = value.split(' ').next();
                    }

                    if name.is_some() && exec.is_some() {
                        break;
                    }
                }

                if let Some((name, exec)) = name.zip(exec) {
                    icons.push(DesktopEntry { name, exec: exec.to_string() });
                }
            }
        }

        DesktopEntries { entries: icons }
    }
}

/// Desktop entry information.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DesktopEntry {
    pub name: String,
    pub exec: String,
}
