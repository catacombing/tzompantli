//! Enumerate installed applications.

use std::{fs, io};

use crossfont::Size;
use linicon::{IconPath, IconType};
use png::DecodingError;
use tiny_skia::{Pixmap, Transform};
use usvg::{FitTo, Options, Tree};
use xdg::BaseDirectories;

use crate::renderer::Texture;
use crate::text::Rasterizer;

/// Desired size for PNG icons.
pub const ICON_SIZE: u32 = 64;

/// List of installed applications.
#[derive(Debug)]
pub struct Apps {
    apps: Vec<App>,
}

impl Apps {
    /// Load all installed applications.
    pub fn new(font: &str, font_size: impl Into<Size>) -> Self {
        // Create font rasterizer.
        let mut rasterizer = Rasterizer::new(font, font_size).expect("Unable to create rasterizer");

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

            let icon_texture = match icon.texture() {
                Ok(texture) => texture,
                Err(_) => continue,
            };

            let text = match rasterizer.rasterize(&entry.name) {
                Ok(texture) => texture,
                Err(_) => continue,
            };

            apps.push(App { icon: icon_texture, exec: entry.exec, text });
        }

        Self { apps }
    }

    /// Iterate over all installed applications.
    pub fn iter(&self) -> impl Iterator<Item = &App> {
        self.apps.iter()
    }

    /// Number of installed applications with icons.
    pub fn len(&self) -> usize {
        self.apps.len()
    }
}

/// Application grid element.
#[derive(Debug)]
pub struct App {
    pub icon: Texture,
    pub text: Texture,
    pub exec: String,
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
struct DesktopEntry {
    name: String,
    exec: String,
}
