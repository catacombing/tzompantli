//! Enumerate installed applications.

use std::collections::HashMap;
use std::path::PathBuf;
use std::{fs, io, slice};

use image::error::ImageError;
use image::imageops::FilterType;
use image::io::Reader as ImageReader;
use tiny_skia::{Pixmap, Transform};
use usvg::{FitTo, Options, Tree};
use xdg::BaseDirectories;

/// Icon lookup paths in reverse order.
const ICON_PATHS: &[(&str, &str)] = &[
    ("/usr/share/icons/hicolor/32x32/apps/", "png"),
    ("/usr/share/icons/hicolor/64x64/apps/", "png"),
    ("/usr/share/icons/hicolor/256x256/apps/", "png"),
    ("/usr/share/icons/hicolor/scalable/apps/", "svg"),
    ("/usr/share/icons/hicolor/128x128/apps/", "png"),
    ("/usr/share/pixmaps/", "svg"),
    ("/usr/share/pixmaps/", "png"),
];

/// Desired size for PNG icons.
pub const ICON_SIZE: u32 = 128;

#[derive(Debug)]
pub struct DesktopEntries {
    entries: Vec<DesktopEntry>,
}

impl DesktopEntries {
    /// Get icons for all installed applications.
    pub fn new() -> Self {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new().expect("Unable to get XDG base directories");
        let dirs = base_dirs.get_data_dirs();

        // Initialize icon loader.
        let icon_loader = IconLoader::new();

        // Find all desktop files in these directories, then look for their icons and
        // executables.
        let mut entries = Vec::new();
        for dir_entry in dirs.iter().flat_map(|d| fs::read_dir(d.join("applications")).ok()) {
            for desktop_file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
                .flat_map(|entry| fs::read_to_string(entry.path()).ok())
            {
                let mut icon = None;
                let mut exec = None;
                let mut name = None;

                for line in desktop_file.lines() {
                    if let Some(value) = line.strip_prefix("Name=") {
                        name = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("Icon=") {
                        icon = icon_loader.load(value).ok();
                    } else if let Some(value) = line.strip_prefix("Exec=") {
                        exec = value.split(' ').next().map(String::from);
                    }

                    if icon.is_some() && exec.is_some() && name.is_some() {
                        break;
                    }
                }

                if let Some(((name, icon), exec)) = name.zip(icon).zip(exec) {
                    entries.push(DesktopEntry { icon, name, exec });
                }
            }
        }

        DesktopEntries { entries }
    }

    /// Create an iterator over all applications.
    pub fn iter(&self) -> slice::Iter<'_, DesktopEntry> {
        self.entries.iter()
    }

    /// Get the desktop entry at the specified index.
    pub fn get(&self, index: usize) -> Option<&DesktopEntry> {
        self.entries.get(index)
    }

    /// Number of installed applications.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Desktop entry information.
#[derive(Debug)]
pub struct DesktopEntry {
    pub icon: Icon,
    pub name: String,
    pub exec: String,
}

/// Rendered icon.
#[derive(Debug, Clone)]
pub struct Icon {
    pub data: Vec<u8>,
    pub width: usize,
}

/// Simple loader for app icons.
struct IconLoader {
    icons: HashMap<String, PathBuf>,
}

impl IconLoader {
    /// Initialize the icon loader.
    ///
    /// This will check all paths for available icons and store them for cheap
    /// lookup.
    fn new() -> Self {
        let mut icons = HashMap::new();

        // Check all paths for icons.
        //
        // Since the `ICON_PATHS` is in reverse order of our priority, we can just
        // insert every new icon into `icons` and it will correctly return the
        // closest match.
        for (path, extension) in ICON_PATHS {
            let mut read_dir = fs::read_dir(path).ok();
            let entries = read_dir.iter_mut().flatten().flatten();
            let files = entries.filter(|e| e.file_type().map_or(false, |e| e.is_file()));

            // Iterate over all files in the directory.
            for file in files {
                let file_name = file.file_name().to_string_lossy().to_string();

                // Store icon paths with the correct extension.
                let name = file_name.rsplit_once('.').filter(|(_, ext)| ext == extension);
                if let Some((name, _)) = name {
                    let _ = icons.insert(name.to_owned(), file.path());
                }
            }
        }

        Self { icons }
    }

    /// Load image file as RGBA buffer.
    fn load(&self, icon: &str) -> Result<Icon, Error> {
        let path = self.icons.get(icon).ok_or(Error::NotFound)?;
        let path_str = path.to_string_lossy();

        match &path_str[path_str.len() - 4..] {
            ".png" => {
                let mut image = ImageReader::open(path)?.decode()?;

                // Resize buffer if needed.
                if image.width() != ICON_SIZE && image.height() != ICON_SIZE {
                    image = image.resize(ICON_SIZE, ICON_SIZE, FilterType::CatmullRom);
                }

                // Premultiply alpha.
                let width = image.width() as usize;
                let mut data = image.into_bytes();
                for chunk in data.chunks_mut(4) {
                    chunk[0] = (chunk[0] as f32 * chunk[3] as f32 / 255.).round() as u8;
                    chunk[1] = (chunk[1] as f32 * chunk[3] as f32 / 255.).round() as u8;
                    chunk[2] = (chunk[2] as f32 * chunk[3] as f32 / 255.).round() as u8;
                }

                Ok(Icon { data, width })
            },
            ".svg" => {
                let resources_dir = Some(path.clone());
                let mut options = Options { resources_dir, ..Options::default() };
                options.fontdb.load_system_fonts();

                let file = fs::read(path)?;
                let tree = Tree::from_data(&file, &options.to_ref())?;

                let mut pixmap = Pixmap::new(ICON_SIZE, ICON_SIZE).ok_or(Error::InvalidSize)?;

                let size = FitTo::Size(ICON_SIZE, ICON_SIZE);
                resvg::render(&tree, size, Transform::default(), pixmap.as_mut())
                    .ok_or(Error::InvalidSize)?;

                let width = pixmap.width() as usize;
                let data = pixmap.take();

                Ok(Icon { data, width })
            },
            _ => unreachable!(),
        }
    }
}

/// Icon loading error.
#[derive(Debug)]
pub enum Error {
    Image(ImageError),
    Svg(usvg::Error),
    Io(io::Error),
    InvalidSize,
    NotFound,
}

impl From<ImageError> for Error {
    fn from(error: ImageError) -> Self {
        Self::Image(error)
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
