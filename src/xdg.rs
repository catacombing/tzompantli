//! Enumerate installed applications.

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use core::arch::aarch64::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{fs, io, iter, slice};

use image::error::ImageError;
use image::imageops::FilterType;
use image::io::Reader as ImageReader;
use xdg::{BaseDirectories, BaseDirectoriesError};

use crate::svg::{self, Svg};

/// Icon name for the placeholder icon.
const PLACEHOLDER_ICON_NAME: &str = "tzompantli-placeholder";

/// Icon lookup paths in reverse order relative to the `$XDG_DATA_DIR`.
const ICON_PATHS: &[(&str, &str)] = &[
    ("pixmaps/", "png"),
    ("icons/hicolor/16x16/apps/", "png"),
    ("icons/hicolor/24x24/apps/", "png"),
    ("icons/hicolor/32x32/apps/", "png"),
    ("icons/hicolor/48x48/apps/", "png"),
    ("icons/hicolor/512x512/apps/", "png"),
    ("icons/hicolor/256x256/apps/", "png"),
    ("icons/hicolor/128x128/apps/", "png"),
    ("pixmaps/", "svg"),
    ("icons/hicolor/scalable/apps/", "svg"),
    ("icons/hicolor/64x64/apps/", "png"),
];

/// Desired size for PNG icons at a scale factor of 1.
const ICON_SIZE: u32 = 64;

#[derive(Debug)]
pub struct DesktopEntries {
    entries: Vec<DesktopEntry>,
    loader: IconLoader,
    scale_factor: f64,
}

impl DesktopEntries {
    /// Get icons for all installed applications.
    pub fn new(scale_factor: f64) -> Result<Self, Error> {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new()?;
        let user_dirs = base_dirs.get_data_home();
        let dirs = base_dirs.get_data_dirs();

        // Initialize icon loader.
        let loader = IconLoader::new(&dirs);

        let mut desktop_entries = DesktopEntries { scale_factor, loader, entries: Vec::new() };
        let icon_size = desktop_entries.icon_size();

        // Create placeholder icon.
        let placeholder_icon = Rc::new(Icon::new_placeholder(icon_size)?);

        // Find all desktop files in these directories, then look for their icons and
        // executables.
        let mut entries = HashMap::new();
        for dir_entry in dirs
            .iter()
            .rev()
            .chain(iter::once(&user_dirs))
            .flat_map(|d| fs::read_dir(d.join("applications")).ok())
        {
            for file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry.file_type().map_or(false, |ft| ft.is_file() || ft.is_symlink())
                })
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
            {
                let desktop_file = match fs::read_to_string(file.path()) {
                    Ok(desktop_file) => desktop_file,
                    Err(_) => continue,
                };

                // Ignore all groups other than the `Desktop Entry` one.
                //
                // Since `Desktop Entry` must be the first group, we just stop at the next group
                // header.
                let lines = desktop_file.lines().take_while(|line| {
                    line.trim_end() == "[Desktop Entry]" || !line.starts_with('[')
                });

                let mut icon = None;
                let mut exec = None;
                let mut name = None;

                // Find name, icon, and executable for the desktop entry.
                for line in lines {
                    // Get K/V pairs, allowing for whitespace around the assignment operator.
                    let (key, value) = match line.split_once('=') {
                        Some((key, value)) => (key.trim_end(), value.trim_start()),
                        None => continue,
                    };

                    match key {
                        "Name" => name = Some(value.to_owned()),
                        "Icon" => icon = desktop_entries.loader.load(value, icon_size).ok(),
                        "Exec" => {
                            // Remove %f/%F/%u/%U/%k variables.
                            let filtered = value
                                .split(' ')
                                .filter(|arg| !matches!(*arg, "%f" | "%F" | "%u" | "%U" | "%k"));
                            exec = Some(filtered.collect::<Vec<_>>().join(" "));
                        },
                        // Ignore explicitly hidden entries.
                        "NoDisplay" if value.trim() == "true" => {
                            exec = None;
                            break;
                        },
                        _ => (),
                    }
                }

                // Hide entries without `Exec=`.
                let exec = match exec {
                    Some(exec) => exec,
                    None => {
                        entries.remove(&file.file_name());
                        continue;
                    },
                };

                if let Some(name) = name {
                    let icon = match icon {
                        Some(icon) => Rc::new(icon),
                        None => placeholder_icon.clone(),
                    };

                    entries.insert(file.file_name(), DesktopEntry { icon, name, exec });
                }
            }
        }
        desktop_entries.entries = entries.into_values().collect();

        // Sort entries for consistent display order.
        desktop_entries.entries.sort_unstable_by(|first, second| first.name.cmp(&second.name));

        Ok(desktop_entries)
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: f64) -> Result<(), Error> {
        // Avoid re-rasterization of icons when factor didn't change.
        if self.scale_factor == scale_factor {
            return Ok(());
        }
        self.scale_factor = scale_factor;

        let icon_size = self.icon_size();

        // Create placeholder icon.
        let placeholder_icon = Rc::new(Icon::new_placeholder(icon_size)?);

        // Update every icon.
        for entry in &mut self.entries {
            if entry.icon.name == PLACEHOLDER_ICON_NAME {
                entry.icon = placeholder_icon.clone();
            } else if let Ok(resized_icon) = self.loader.load(&entry.icon.name, icon_size) {
                entry.icon = Rc::new(resized_icon);
            }
        }

        Ok(())
    }

    /// Desktop icon size.
    pub fn icon_size(&self) -> u32 {
        (ICON_SIZE as f64 * self.scale_factor).round() as u32
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
    pub icon: Rc<Icon>,
    pub name: String,
    pub exec: String,
}

/// Rendered icon.
#[derive(Debug, Clone)]
pub struct Icon {
    pub data: Vec<u8>,
    pub width: usize,
    name: String,
}

impl Icon {
    /// Create new "missing icon" icon.
    fn new_placeholder(size: u32) -> Result<Self, Error> {
        const PLACEHOLDER_SVG: &[u8] = include_bytes!("../svgs/placeholder.svg");
        let mut placeholder = Svg::parse(PLACEHOLDER_SVG)?;
        let (data, width) = placeholder.render(size)?;
        Ok(Icon { data: data.to_vec(), width: width as usize, name: PLACEHOLDER_ICON_NAME.into() })
    }
}

/// Simple loader for app icons.
#[derive(Debug)]
struct IconLoader {
    icons: HashMap<String, PathBuf>,
}

impl IconLoader {
    /// Initialize the icon loader.
    ///
    /// This will check all paths for available icons and store them for cheap
    /// lookup.
    fn new(data_dirs: &[PathBuf]) -> Self {
        let mut icons = HashMap::new();

        // Check all paths for icons.
        //
        // Since the `ICON_PATHS` is in reverse order of our priority, we can just
        // insert every new icon into `icons` and it will correctly return the
        // closest match.
        for (path, extension) in data_dirs
            .iter()
            .flat_map(|base| ICON_PATHS.iter().map(|(path, ext)| (base.join(path), ext)))
        {
            let mut read_dir = fs::read_dir(path).ok();
            let entries = read_dir.iter_mut().flatten().flatten();
            let files =
                entries.filter(|e| e.file_type().map_or(false, |e| e.is_file() || e.is_symlink()));

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

    fn premultiply_generic(data: &mut [u8]) {
        // TODO: change to array_chunks_mut() once that is stabilised.
        for chunk in data.chunks_exact_mut(4) {
            if let [r, g, b, a] = chunk {
                let r = *r as u16 * *a as u16 + 127;
                let g = *g as u16 * *a as u16 + 127;
                let b = *b as u16 * *a as u16 + 127;
                chunk[0] = ((r + (r >> 8) + 1) >> 8) as u8;
                chunk[1] = ((g + (g >> 8) + 1) >> 8) as u8;
                chunk[2] = ((b + (b >> 8) + 1) >> 8) as u8;
            }
        }
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    fn premultiply_aarch64(data: &mut [u8]) {
        // Work on “just” 8 pixels at once, since we need the full 16-bytes of
        // the q registers for the multiplication.
        //
        // TODO: change to array_chunks_mut() once that is stabilised.
        let mut iter = data.chunks_exact_mut(8 * 4);

        unsafe {
            let one = vdupq_n_u16(1);
            let half = vdupq_n_u16(127);

            while let Some(chunk) = iter.next() {
                let chunk = chunk.as_mut_ptr();
                let uint8x8x4_t(mut r8, mut g8, mut b8, a8) = vld4_u8(chunk);

                // This is the same algorithm as the other premultiply(), but on
                // packed 16-bit instead of float.

                let mut r16 = vmull_u8(r8, a8);
                let mut g16 = vmull_u8(g8, a8);
                let mut b16 = vmull_u8(b8, a8);

                r16 = vaddq_u16(r16, half);
                g16 = vaddq_u16(g16, half);
                b16 = vaddq_u16(b16, half);

                r16 = vsraq_n_u16(r16, r16, 8);
                g16 = vsraq_n_u16(g16, g16, 8);
                b16 = vsraq_n_u16(b16, b16, 8);

                r16 = vaddq_u16(r16, one);
                g16 = vaddq_u16(g16, one);
                b16 = vaddq_u16(b16, one);

                r8 = vshrn_n_u16(r16, 8);
                g8 = vshrn_n_u16(g16, 8);
                b8 = vshrn_n_u16(b16, 8);

                vst4_u8(chunk, uint8x8x4_t(r8, g8, b8, a8));
            }
        }

        // Use generic fallback for the pixels not evenly divisible by our vector size.
        Self::premultiply_generic(iter.into_remainder());
    }

    /// Load image file as RGBA buffer.
    fn load(&self, icon: &str, size: u32) -> Result<Icon, Error> {
        let name = icon.into();

        // Resolve icon from name if it is not an absolute path.
        let mut path = Path::new(icon);
        if !path.is_absolute() {
            path = self.icons.get(icon).ok_or(Error::NotFound)?;
        }

        match path.extension().and_then(|ext| ext.to_str()) {
            Some("png") => {
                let mut image = ImageReader::open(path)?.decode()?;

                // Resize buffer if needed.
                if image.width() != size && image.height() != size {
                    image = image.resize(size, size, FilterType::CatmullRom);
                }

                // Premultiply alpha.
                let width = image.width() as usize;
                let mut data = image.into_rgba8().into_raw();

                #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
                Self::premultiply_aarch64(&mut data);
                #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
                Self::premultiply_generic(&mut data);

                Ok(Icon { data, width, name })
            },
            Some("svg") => {
                let mut svg = Svg::from_path(path)?;
                let (data, width) = svg.render(size)?;
                Ok(Icon { data: data.to_vec(), width: width as usize, name })
            },
            _ => unreachable!(),
        }
    }
}

/// Icon loading error.
#[derive(Debug)]
pub enum Error {
    BaseDirectories(BaseDirectoriesError),
    Image(ImageError),
    Svg(svg::Error),
    Io(io::Error),
    NotFound,
}

impl From<BaseDirectoriesError> for Error {
    fn from(error: BaseDirectoriesError) -> Self {
        Self::BaseDirectories(error)
    }
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

impl From<svg::Error> for Error {
    fn from(error: svg::Error) -> Self {
        Self::Svg(error)
    }
}
