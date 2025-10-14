//! Enumerate installed applications.

use core::cmp::{self, Ordering};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::{fs, io};

use tracing::error;
use xdg::BaseDirectories;

use crate::Error;

/// Placeholder icon SVG.
const PLACEHOLDER_SVG: &[u8] = include_bytes!("../svgs/placeholder.svg");
/// Poweroff entry icon SVG.
const POWEROFF_SVG: &[u8] = include_bytes!("../svgs/poweroff.svg");
/// Config entry icon SVG.
const CONFIG_SVG: &[u8] = include_bytes!("../svgs/config.svg");
/// Reboot entry icon SVG.
const REBOOT_SVG: &[u8] = include_bytes!("../svgs/reboot.svg");
/// Hidden entry icon SVG.
const HIDDEN_SVG: &[u8] = include_bytes!("../svgs/hidden.svg");

#[derive(Debug)]
pub struct DesktopEntries {
    entries: Vec<DesktopEntry>,
    loader: IconLoader,
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

        // Configure builtin icons.
        let entries = vec![
            DesktopEntry {
                name: Arc::new("Poweroff".into()),
                exec: ExecAction::Poweroff,
                ..Default::default()
            },
            DesktopEntry {
                name: Arc::new("Tap App".into()),
                exec: ExecAction::Config,
                ..Default::default()
            },
            DesktopEntry {
                name: Arc::new("Reboot".into()),
                exec: ExecAction::Reboot,
                ..Default::default()
            },
        ];

        let mut desktop_entries = DesktopEntries { entries, loader };

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
                        icon_name,
                        exec: ExecAction::Run(exec),
                        filename: file.file_name(),
                        name: Arc::new(name),
                        hidden_paths: Default::default(),
                        grid_index: Default::default(),
                    });
                }
            }
        }
        desktop_entries.entries.extend(entries.into_values());

        // Sort entries for consistent display order.
        desktop_entries.entries.sort_unstable_by(|first, second| {
            first.exec.partial_cmp(&second.exec).unwrap_or_else(|| first.name.cmp(&second.name))
        });

        Ok(desktop_entries)
    }

    /// Get icon for a dekstop entry.
    pub fn icon(&self, entry: &DesktopEntry, size: u32) -> Icon {
        self.icon_internal(entry, size).unwrap_or(Icon::new_svg(IconIdentifier::Placeholder))
    }

    /// Attempt to load an icon.
    ///
    /// If no icon can be found, `None` will be returned and the placeholder
    /// icon should be used instead.
    fn icon_internal(&self, entry: &DesktopEntry, size: u32) -> Option<Icon> {
        // Handle builtin icons.
        match (entry.hidden(), &entry.exec) {
            (false, ExecAction::Poweroff) => return Some(Icon::new_svg(IconIdentifier::Poweroff)),
            (false, ExecAction::Config) => return Some(Icon::new_svg(IconIdentifier::Config)),
            (false, ExecAction::Reboot) => return Some(Icon::new_svg(IconIdentifier::Reboot)),
            (true, _) => return Some(Icon::new_svg(IconIdentifier::Hidden)),
            _ => (),
        }

        let icon_name = entry.icon_name.as_ref()?;

        // Resolve icon from name if it is not an absolute path.
        let mut path = PathBuf::from(icon_name);
        if !path.is_absolute() {
            path = self.loader.icon_path(icon_name, size)?.into();
        }

        let icon_type = match path.extension().and_then(|ext| ext.to_str()) {
            Some("png") => IconType::Png,
            Some("svg") | Some("svgz") => IconType::Svg,
            ext => {
                error!("Invalid icon extension for {}: {ext:?}", entry.name);
                return None;
            },
        };

        Some(Icon { icon_type, identifier: IconIdentifier::Path(path) })
    }

    /// Create an iterator over all enabled applications.
    pub fn visible(&self) -> impl Iterator<Item = &DesktopEntry> {
        self.entries.iter().filter(|entry| !entry.hidden())
    }

    /// Get immutable access to all desktop entries.
    pub fn all(&self) -> &[DesktopEntry] {
        &self.entries
    }

    /// Get mutable access to all desktop entries.
    pub fn all_mut(&mut self) -> &mut [DesktopEntry] {
        &mut self.entries
    }

    /// Get reference to index out of all visible and hidden applications.
    pub fn all_get(&self, index: usize) -> Option<&DesktopEntry> {
        self.entries.get(index)
    }

    /// Get mutable reference to index out of all visible and hidden
    /// applications.
    pub fn all_get_mut(&mut self, index: usize) -> Option<&mut DesktopEntry> {
        self.entries.get_mut(index)
    }

    /// Get total number of visible and hidden applications
    pub fn all_len(&self) -> usize {
        self.entries.len()
    }

    /// Remove a desktop entry.
    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
    }
}

/// Desktop entry information.
#[derive(Default, Debug)]
pub struct DesktopEntry {
    pub icon_name: Option<String>,
    pub name: Arc<String>,
    pub exec: ExecAction,

    // Grid index cache used during rendering.
    pub grid_index: Option<usize>,

    hidden_paths: Vec<PathBuf>,
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

/// Desktop entry icon data.
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
pub struct Icon {
    identifier: IconIdentifier,
    icon_type: IconType,
}

impl Icon {
    /// Create a new placeholder icon.
    fn new_svg(identifier: IconIdentifier) -> Self {
        Self { identifier, icon_type: IconType::Svg }
    }

    /// Load the data associated with this icon.
    pub fn load(&self) -> Cow<'static, [u8]> {
        match &self.identifier {
            IconIdentifier::Path(path) => match read_to_vec(path) {
                Ok(data) => Cow::Owned(data),
                Err(err) => {
                    error!("Failed to read svg: {err}");
                    Cow::Borrowed(PLACEHOLDER_SVG)
                },
            },
            IconIdentifier::Placeholder => Cow::Borrowed(PLACEHOLDER_SVG),
            IconIdentifier::Poweroff => Cow::Borrowed(POWEROFF_SVG),
            IconIdentifier::Config => Cow::Borrowed(CONFIG_SVG),
            IconIdentifier::Reboot => Cow::Borrowed(REBOOT_SVG),
            IconIdentifier::Hidden => Cow::Borrowed(HIDDEN_SVG),
        }
    }

    /// Get icon data format.
    pub fn icon_type(&self) -> IconType {
        self.icon_type
    }
}

/// Type of desktop entry icons.
#[derive(Hash, PartialEq, Eq, Copy, Clone, Debug)]
pub enum IconType {
    Svg,
    Png,
}

/// Unique desktop entry icon identifier.
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
enum IconIdentifier {
    Path(PathBuf),
    Placeholder,
    Poweroff,
    Config,
    Reboot,
    Hidden,
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
    fn icon_path<'a>(&'a self, icon: &str, size: u32) -> Option<&'a Path> {
        // Get all available icons matching this icon name.
        let icons = &self.icons.get(icon)?.1;
        let mut icons = icons.iter();

        // Initialize accumulator with the first iterator item.
        let mut ideal_icon = match icons.next() {
            // Short-circuit if the first icon is an exact match.
            Some((ImageType::SizedBitmap(icon_size), path)) if *icon_size == size => {
                return Some(path.as_path());
            },
            Some(first_icon) => first_icon,
            None => return None,
        };

        // Find the ideal icon.
        for icon in icons {
            // Short-circuit if an exact size match exists.
            if matches!(icon, (ImageType::SizedBitmap(icon_size), _) if *icon_size == size) {
                return Some(icon.1);
            }

            // Otherwise find closest match.
            ideal_icon = cmp::max(icon, ideal_icon);
        }

        Some(ideal_icon.1.as_path())
    }
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

/// Launcher action.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub enum ExecAction {
    #[default]
    Poweroff,
    Config,
    Reboot,
    Run(String),
}

impl PartialOrd for ExecAction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            return Some(Ordering::Equal);
        }

        match (self, other) {
            (Self::Config, Self::Poweroff)
            | (Self::Reboot, Self::Poweroff | Self::Config)
            | (Self::Run(_), Self::Poweroff | Self::Config | Self::Reboot) => {
                Some(Ordering::Greater)
            },
            (Self::Run(_), Self::Run(_)) => None,
            _ => Some(Ordering::Less),
        }
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

/// Read all the bytes in a file.
fn read_to_vec(path: &Path) -> Result<Vec<u8>, io::Error> {
    let mut file = File::open(path)?;

    // Create a vec with its capacity matching the file size.
    let mut data = Vec::new();
    if let Ok(metadata) = file.metadata() {
        data.reserve_exact(metadata.len() as usize);
    }

    file.read_to_end(&mut data)?;

    Ok(data)
}
