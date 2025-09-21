//! Enumerate installed applications.

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use core::arch::aarch64::*;
use core::cmp::{self, Ordering};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::{fs, io};

use image::ImageReader;
use image::error::ImageError;
use image::imageops::FilterType;
use xdg::BaseDirectories;

use crate::svg::{self, Svg};

/// Placeholder icon SVG.
const PLACEHOLDER_SVG: &[u8] = include_bytes!("../svgs/placeholder.svg");
/// Hidden entry icon SVG.
const HIDDEN_SVG: &[u8] = include_bytes!("../svgs/hidden.svg");

/// Desired size for PNG icons at a scale factor of 1.
const ICON_SIZE: u32 = 64;

#[derive(Debug)]
pub struct DesktopEntries {
    entries: Vec<DesktopEntry>,
    loader: IconLoader,
    scale_factor: f64,

    rendered_placeholder: Option<Rc<Icon>>,
    placeholder: Svg,
    rendered_hidden: Option<Rc<Icon>>,
    hidden: Svg,
}

impl DesktopEntries {
    /// Get icons for all installed applications.
    pub fn new() -> Result<Self, Error> {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new();
        let user_dirs = base_dirs.get_data_home();
        let dirs = base_dirs.get_data_dirs();

        // Initialize icon loader.
        let loader = IconLoader::new(&dirs);

        // Create placeholder/hidden icons.
        let placeholder = Svg::parse(PLACEHOLDER_SVG)?;
        let hidden = Svg::parse(HIDDEN_SVG)?;

        let mut desktop_entries = DesktopEntries {
            placeholder,
            hidden,
            loader,
            rendered_placeholder: Default::default(),
            rendered_hidden: Default::default(),
            scale_factor: Default::default(),
            entries: Default::default(),
        };

        // Find all desktop files in these directories, then look for their icons and
        // executables.
        let mut entries: HashMap<OsString, DesktopEntry> = HashMap::new();
        for dir_entry in dirs
            .iter()
            .rev()
            .chain(&user_dirs)
            .flat_map(|d| fs::read_dir(d.join("applications")).ok())
        {
            for file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_file() || ft.is_symlink()))
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

                let mut icon_name = None;
                let mut hidden = false;
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
                        "Icon" => icon_name = Some(value.to_owned()),
                        "Exec" => {
                            // Remove %f/%F/%u/%U/%k variables.
                            let filtered = value
                                .split(' ')
                                .filter(|arg| !matches!(*arg, "%f" | "%F" | "%u" | "%U" | "%k"));
                            exec = Some(filtered.collect::<Vec<_>>().join(" "));
                        },
                        // Ignore explicitly hidden entries.
                        "NoDisplay" if value.trim() == "true" => {
                            hidden = true;
                            break;
                        },
                        _ => (),
                    }
                }

                // Mark entries with `NoDisplay` or without `Exec` as hidden.
                let exec = match exec {
                    // Store paths for explicitly hidden applications.
                    _ if hidden => {
                        if let Some(entry) = entries.get_mut(&file.file_name()) {
                            entry.hidden_paths.push(file.path());
                        }
                        continue;
                    },
                    Some(exec) => exec,
                    // Ignore non-executable desktop files.
                    None => {
                        entries.remove(&file.file_name());
                        continue;
                    },
                };

                if let Some(name) = name {
                    entries.insert(file.file_name(), DesktopEntry {
                        filename: file.file_name(),
                        icon_name,
                        name,
                        exec,
                        hidden_paths: Default::default(),
                        icon_source: Default::default(),
                        icon: Default::default(),
                    });
                }
            }
        }
        desktop_entries.entries = entries.into_values().collect();

        // Sort entries for consistent display order.
        desktop_entries.entries.sort_unstable_by(|first, second| first.name.cmp(&second.name));

        Ok(desktop_entries)
    }

    /// Update the DPI scale factor.
    pub fn render_at_scale_factor(
        &mut self,
        scale_factor: f64,
        render_hidden: bool,
    ) -> Result<(), Error> {
        self.scale_factor = scale_factor;

        let icon_size = self.icon_size();

        // Rasterize placeholders if necessary.
        if self.rendered_placeholder.as_ref().is_none_or(|icon| icon.width != icon_size as usize) {
            let (data, width) = self.placeholder.render(icon_size)?;
            self.rendered_placeholder =
                Some(Rc::new(Icon { data: data.to_vec(), width: width as usize }));

            let (data, width) = self.hidden.render(icon_size)?;
            self.rendered_hidden =
                Some(Rc::new(Icon { data: data.to_vec(), width: width as usize }));
        }
        let placeholder_icon = self.rendered_placeholder.as_ref().unwrap();
        let hidden_icon = self.rendered_hidden.as_ref().unwrap();

        let entries: Box<dyn Iterator<Item = &mut DesktopEntry>> = if render_hidden {
            Box::new(self.entries.iter_mut())
        } else {
            Box::new(self.entries.iter_mut().filter(|entry| !entry.hidden()))
        };

        // Update every icon.
        for entry in entries {
            let source = if entry.hidden() {
                IconSource::Hidden
            } else if entry.icon_name.is_some() {
                IconSource::Xdg
            } else {
                IconSource::Placeholder
            };

            // Skip icons that are already up to date.
            if entry.icon.as_ref().is_some_and(|icon| icon.width == icon_size as usize)
                && entry.icon_source == Some(source)
            {
                continue;
            }

            entry.icon = Some(match &entry.icon_name {
                _ if entry.hidden() => hidden_icon.clone(),
                None => placeholder_icon.clone(),
                Some(icon_name) => match self.loader.load(icon_name, icon_size) {
                    Ok(icon) => Rc::new(icon),
                    // Fallback to placeholder if rendering failed.
                    //
                    // We still set the icon source to Xdg, since we want to cache it as the 'real'
                    // icon if we know that attempting to render it would just fail again.
                    Err(err) => {
                        eprintln!("Failed to render icon {icon_name}: {err:?}");
                        placeholder_icon.clone()
                    },
                },
            });

            entry.icon_source = Some(source);
        }

        Ok(())
    }

    /// Desktop icon size.
    pub fn icon_size(&self) -> u32 {
        (ICON_SIZE as f64 * self.scale_factor).round() as u32
    }

    /// Create an iterator over all enabled applications.
    pub fn visible(&self) -> impl Iterator<Item = &DesktopEntry> {
        self.entries.iter().filter(|entry| !entry.hidden())
    }

    /// Create an iterator over visible and hidden applications.
    pub fn all(&self) -> impl Iterator<Item = &DesktopEntry> {
        self.entries.iter()
    }

    /// Create an iterator over visible and hidden applications.
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut DesktopEntry> {
        self.entries.iter_mut()
    }

    /// Remove a desktop entry.
    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
    }
}

/// Desktop entry information.
#[derive(Debug)]
pub struct DesktopEntry {
    pub hidden_paths: Vec<PathBuf>,
    pub icon_name: Option<String>,
    pub icon: Option<Rc<Icon>>,
    pub name: String,
    pub exec: String,

    icon_source: Option<IconSource>,
    filename: OsString,
}

impl DesktopEntry {
    /// Toggle the hidden status of the desktop entry.
    pub fn toggle_hidden(&mut self) -> io::Result<()> {
        if self.hidden_paths.is_empty() { self.set_hidden() } else { self.set_visible() }
    }

    /// Remove `NoDisplay` flag from all known desktop entry files.
    fn set_visible(&mut self) -> io::Result<()> {
        for path in self.hidden_paths.drain(..) {
            let mut content = fs::read_to_string(&path)?;

            // Remove entry if it only contains `NoDisplay`, or edit it otherwise.
            if content.trim() == "NoDisplay=true" {
                fs::remove_file(&path)?;
            } else if content.contains("NoDisplay=true") {
                content = content.replace("NoDisplay=true", "NoDisplay=false");
                fs::write(&path, content)?;
            }
        }
        Ok(())
    }

    /// Add `NoDisplay` flag to desktop file in user's home dir.
    fn set_hidden(&mut self) -> io::Result<()> {
        // Get path of the `~/.local/share/applications` directory.
        let data_home = match BaseDirectories::new().get_data_home() {
            Some(data_home) => data_home,
            None => return Err(io::Error::other("missing user data home")),
        };
        let apps_dir = data_home.join("applications");

        // Ensure directory exists.
        fs::create_dir_all(&apps_dir)?;

        // Edit or create the desktop entry.
        let file_path = apps_dir.join(&self.filename);
        match fs::read_to_string(&file_path) {
            Ok(content) => {
                let content = if content.contains("NoDisplay=false") {
                    content.replace("NoDisplay=false", "NoDisplay=true")
                } else {
                    format!("{content}\nNoDisplay=true")
                };
                fs::write(&file_path, content)?;
            },
            // Create file if it does not exist yet.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::write(&file_path, "NoDisplay=true\n")?;
            },
            Err(err) => return Err(err),
        }

        // Mark entry as hidden.
        self.hidden_paths.push(file_path);

        Ok(())
    }

    /// Check whether the desktop entry is marked as `NoDisplay`.
    pub fn hidden(&self) -> bool {
        !self.hidden_paths.is_empty()
    }
}

/// Rendered icon.
#[derive(Debug, Clone)]
pub struct Icon {
    pub data: Vec<u8>,
    pub width: usize,
}

/// Expected type of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ImageType {
    /// A bitmap image of a known square size.
    SizedBitmap(u32),

    /// A bitmap image of an unknown size.
    Bitmap,

    /// A vector image.
    Scalable,

    /// A monochrome vector image.
    Symbolic,
}

impl Ord for ImageType {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }

        match (self, other) {
            // Prefer scaleable formats.
            (Self::Scalable, _) => Ordering::Greater,
            (_, Self::Scalable) => Ordering::Less,
            // Prefer bigger bitmap sizes.
            (Self::SizedBitmap(size), Self::SizedBitmap(other_size)) => size.cmp(other_size),
            // Prefer bitmaps with known size.
            (Self::SizedBitmap(_), _) => Ordering::Greater,
            (_, Self::SizedBitmap(_)) => Ordering::Less,
            // Prefer bitmaps over symbolic icons without color.
            (Self::Bitmap, _) => Ordering::Greater,
            (_, Self::Bitmap) => Ordering::Less,
            // Equality is checked by the gate clause already.
            (Self::Symbolic, Self::Symbolic) => unreachable!(),
        }
    }
}

impl PartialOrd for ImageType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Simple loader for app icons.
#[derive(Debug)]
struct IconLoader {
    icons: HashMap<String, (String, HashMap<ImageType, PathBuf>)>,
}

impl IconLoader {
    /// Initialize the icon loader.
    ///
    /// This will check all paths for available icons and store them for cheap
    /// lookup.
    fn new(data_dirs: &[PathBuf]) -> Self {
        let mut icons: HashMap<String, (String, HashMap<ImageType, PathBuf>)> = HashMap::new();

        // NOTE: Themes are checked in order of priority, if an icon is found in a theme
        // of lesser priority, it is ignored completely regardless of how low
        // quality the existing icon might be.

        // Iterate on all XDG_DATA_DIRS to look for icons.
        for data_dir in data_dirs {
            // Iterate over theme fallback list in descending importance.
            for theme in themes_for_dir(&data_dir.join("icons")) {
                let theme_dir = data_dir.join("icons").join(&theme);
                for dir_entry in fs::read_dir(&theme_dir).into_iter().flatten().flatten() {
                    // Get last path segment from directory.
                    let dir_name = match dir_entry.file_name().into_string() {
                        Ok(dir_name) => dir_name,
                        Err(_) => continue,
                    };

                    // Handle standardized icon theme directory layout.
                    let image_type = if dir_name == "scalable" {
                        ImageType::Scalable
                    } else if dir_name == "symbolic" {
                        ImageType::Symbolic
                    } else if let Some((width, height)) = dir_name.split_once('x') {
                        match (u32::from_str(width), u32::from_str(height)) {
                            (Ok(width), Ok(height)) if width == height => {
                                ImageType::SizedBitmap(width)
                            },
                            _ => continue,
                        }
                    } else {
                        continue;
                    };

                    // Iterate over all files in all category subdirectories.
                    let categories = fs::read_dir(dir_entry.path()).into_iter().flatten().flatten();
                    for file in categories.flat_map(|c| fs::read_dir(c.path())).flatten().flatten()
                    {
                        // Get last path segment from file.
                        let file_name = match file.file_name().into_string() {
                            Ok(file_name) => file_name,
                            Err(_) => continue,
                        };

                        // Strip extension.
                        let name = match (file_name.rsplit_once('.'), image_type) {
                            (Some(("", _)), _) => continue,
                            (Some((name, _)), ImageType::Symbolic) => {
                                match name.strip_suffix("-symbolic") {
                                    Some(name) => name,
                                    None => continue,
                                }
                            },
                            (Some((name, _)), _) => name,
                            (None, _) => continue,
                        };

                        // Ignore new icon if icon from higher priority theme exists.
                        let icons = match icons.entry(name.to_owned()) {
                            Entry::Occupied(entry) => {
                                let (existing_theme, icons) = entry.into_mut();
                                if existing_theme == &theme {
                                    icons
                                } else {
                                    continue;
                                }
                            },
                            Entry::Vacant(entry) => {
                                &mut entry.insert((theme.clone(), HashMap::new())).1
                            },
                        };

                        // Add icon to our icon loader.
                        icons.insert(image_type, file.path());
                    }
                }
            }
        }

        // Add pixmaps first, this path is hardcoded in the specification.
        for file in fs::read_dir("/usr/share/pixmaps").into_iter().flatten().flatten() {
            // Get last path segment from file.
            let file_name = match file.file_name().into_string() {
                Ok(file_name) => file_name,
                Err(_) => continue,
            };

            // Determine image type based on extension.
            let (name, image_type) = match file_name.rsplit_once('.') {
                Some((name, "svg")) => (name, ImageType::Scalable),
                // We don’t have any information about the size of the icon here.
                Some((name, "png")) => (name, ImageType::Bitmap),
                _ => continue,
            };

            // Add icon to our icon loader.
            if !icons.contains_key(name) {
                icons.entry(name.to_owned()).or_default().1.insert(image_type, file.path());
            }
        }

        Self { icons }
    }

    /// Get the ideal icon for a specific size.
    fn icon_path<'a>(&'a self, icon: &str, size: u32) -> Result<&'a Path, Error> {
        // Get all available icons matching this icon name.
        let icons = &self.icons.get(icon).ok_or(Error::NotFound)?.1;
        let mut icons = icons.iter();

        // Initialize accumulator with the first iterator item.
        let mut ideal_icon = match icons.next() {
            // Short-circuit if the first icon is an exact match.
            Some((ImageType::SizedBitmap(icon_size), path)) if *icon_size == size => {
                return Ok(path.as_path());
            },
            Some(first_icon) => first_icon,
            None => return Err(Error::NotFound),
        };

        // Find the ideal icon.
        for icon in icons {
            // Short-circuit if an exact size match exists.
            if matches!(icon, (ImageType::SizedBitmap(icon_size), _) if *icon_size == size) {
                return Ok(icon.1);
            }

            // Otherwise find closest match.
            ideal_icon = cmp::max(icon, ideal_icon);
        }

        Ok(ideal_icon.1.as_path())
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
    fn load(&mut self, icon: &str, size: u32) -> Result<Icon, Error> {
        // Resolve icon from name if it is not an absolute path.
        let mut path = Path::new(icon);
        if !path.is_absolute() {
            path = self.icon_path(icon, size)?;
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

                Ok(Icon { data, width })
            },
            Some("svg") | Some("svgz") => {
                let mut svg = Svg::from_path(path)?;
                let (data, width) = svg.render(size)?;
                Ok(Icon { data: data.to_vec(), width: width as usize })
            },
            _ => unreachable!(),
        }
    }
}

/// Icon loading error.
#[allow(dead_code)]
#[derive(Debug)]
pub enum Error {
    Image(ImageError),
    Svg(svg::Error),
    Io(io::Error),
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

impl From<svg::Error> for Error {
    fn from(error: svg::Error) -> Self {
        Self::Svg(error)
    }
}

/// Recursively parse theme specs to find theme fallback hierarchy.
fn themes_for_dir(root_dir: &Path) -> Vec<String> {
    let mut all_themes = vec!["default".into()];
    let mut index = 0;

    while index < all_themes.len() {
        // Add theme's dependencies to theme list.
        let index_path = root_dir.join(&all_themes[index]).join("index.theme");
        let mut themes = parse_index(&index_path);
        all_themes.append(&mut themes);

        // Deduplicate themes list, to avoid redundant work.
        for i in (0..all_themes.len()).rev() {
            if all_themes[..i].contains(&all_themes[i]) {
                all_themes.remove(i);
            }
        }

        index += 1;
    }

    all_themes
}

/// Parse index.theme and extract `Inherits` attribute.
fn parse_index(path: &Path) -> Vec<String> {
    // Read entire file.
    let index = match fs::read_to_string(path) {
        Ok(index) => index,
        Err(_) => return Vec::new(),
    };

    // Find `Inherits` attribute start.
    let start = match index.find("Inherits=") {
        Some(start) => start + "Inherits=".len(),
        None => return Vec::new(),
    };

    // Extract `Inherits` value.
    let inherits = match index[start..].find(char::is_whitespace) {
        Some(end) => &index[start..start + end],
        None => &index[start..],
    };

    inherits.split(',').map(|s| s.to_string()).collect()
}

/// Types of renderable icons.
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum IconSource {
    /// Builtin placeholder icon.
    Placeholder,
    /// Builtin hidden entry icon.
    Hidden,
    /// Desktop entry icon.
    Xdg,
}
