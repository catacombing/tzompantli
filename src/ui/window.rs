//! Wayland window rendering.

use std::collections::HashMap;
use std::process::Command;
use std::ptr::NonNull;
use std::sync::Arc;
use std::{mem, process};

use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
use rayon::prelude::*;
use resvg::tiny_skia::Pixmap as SvgPixmap;
use resvg::usvg::{Options as SvgOptions, Transform as SvgTransform, Tree as SvgTree};
use skia_safe::image::images;
use skia_safe::textlayout::{
    FontCollection, ParagraphBuilder, ParagraphStyle, TextAlign, TextStyle,
};
use skia_safe::{
    AlphaType, Canvas as SkiaCanvas, ColorType, Data, FilterMode, FontMgr, IRect, Image, ImageInfo,
    MipmapMode, Paint, Rect, SamplingOptions,
};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{Window as XdgWindow, WindowDecorations};
use tracing::error;

use crate::config::Config;
use crate::geometry::{Point, Size};
use crate::ui::ScrollVelocity;
use crate::ui::renderer::Renderer;
use crate::ui::skia::Canvas;
use crate::wayland::ProtocolStates;
use crate::xdg::{DesktopEntries, DesktopEntry, ExecAction, Icon, IconType};
use crate::{Error, State, dbus};

/// Height of a desktop entry at scale 1.
const ENTRY_WIDTH: u32 = 96;

/// Height of a desktop entry at scale 1.
const ENTRY_HEIGHT: u32 = 112;

/// Minimum padding around entries at scale 1.
const MIN_PADDING: f64 = 8.;

/// Desktop entry icon size at scale 1.
const ICON_SIZE: f64 = 64.;

/// Wayland window.
pub struct Window {
    pub queue: QueueHandle<State>,
    pub initial_draw_done: bool,

    entries: DesktopEntries,
    configuring: bool,
    config: Config,

    velocity: ScrollVelocity,
    touch_state: TouchState,
    scroll_offset: f64,

    connection: Connection,
    xdg_window: XdgWindow,
    viewport: WpViewport,

    render_cache: RenderCache,
    renderer: Renderer,
    canvas: Canvas,

    stalled: bool,
    dirty: bool,
    size: Size,
    scale: f64,
}

impl Window {
    pub fn new(
        protocol_states: &ProtocolStates,
        connection: Connection,
        queue: QueueHandle<State>,
        config: Config,
    ) -> Result<Self, Error> {
        // Get EGL display.
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        let wayland_display = WaylandDisplayHandle::new(display);
        let raw_display = RawDisplayHandle::Wayland(wayland_display);
        let egl_display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl)? };

        // Create surface's Wayland global handles.
        let surface = protocol_states.compositor.create_surface(&queue);
        if let Some(fractional_scale) = &protocol_states.fractional_scale {
            fractional_scale.fractional_scaling(&queue, &surface);
        }
        let viewport = protocol_states.viewporter.viewport(&queue, &surface);

        // Create the XDG shell window.
        let xdg_window = protocol_states.xdg_shell.create_window(
            surface.clone(),
            WindowDecorations::RequestClient,
            &queue,
        );
        xdg_window.set_title("Tzompantli");
        xdg_window.set_app_id("Tzompantli");
        xdg_window.commit();

        // Create OpenGL renderer.
        let renderer = Renderer::new(egl_display, surface);

        // Default to a reasonable default size.
        let size = Size { width: 360, height: 720 };

        // Lookup available applications.
        let entries = DesktopEntries::new().expect("Unable to load desktop entries");

        let render_cache = RenderCache::new(&config);

        Ok(Self {
            render_cache,
            connection,
            xdg_window,
            viewport,
            renderer,
            entries,
            config,
            queue,
            size,
            stalled: true,
            dirty: true,
            scale: 1.,
            initial_draw_done: Default::default(),
            scroll_offset: Default::default(),
            configuring: Default::default(),
            touch_state: Default::default(),
            velocity: Default::default(),
            canvas: Default::default(),
        })
    }

    /// Redraw the window.
    pub fn draw(&mut self) {
        // Stall rendering if nothing changed since last redraw.
        if !mem::take(&mut self.dirty) && !self.velocity.is_moving() {
            self.stalled = true;
            return;
        }
        self.initial_draw_done = true;

        // Animate scroll velocity.
        self.velocity.apply(&self.config.input, &mut self.scroll_offset);

        // Ensure offset is correct in case alarms were deleted or geometry changed.
        self.clamp_scroll_offset();

        // Update viewporter logical render size.
        //
        // NOTE: This must be done every time we draw with Sway; it is not
        // persisted when drawing with the same surface multiple times.
        let physical_size = self.size * self.scale;
        self.viewport.set_source(0., 0., physical_size.width as f64, physical_size.height as f64);
        self.viewport.set_destination(self.size.width as i32, self.size.height as i32);

        // Mark entire window as damaged.
        let wl_surface = self.xdg_window.wl_surface();
        wl_surface.damage(0, 0, self.size.width as i32, self.size.height as i32);

        // Our render cache stores images by making snapshots of the rendered surface,
        // which allows rendering without an offscreen canvas. Snapshots however
        // can only cover the visible region of the surface, which would usually
        // prevent us from caching partially visible entries.
        //
        // To ensure entries are always rendered in their entirety, so we can cache
        // them, we add an offscreen padding at the bottom of our surface that
        // has enough space for one more row of entries. This offscreen padding
        // is then excluded from the rendered content using viewporter.
        let grid = Grid::new(physical_size, self.scale);
        let entry_height = grid.entry_size.height + grid.padding;
        let mut surface_size = physical_size;
        surface_size.height += entry_height;

        // Render the window content.
        self.renderer.draw(surface_size, |renderer| {
            self.canvas.draw(renderer.skia_config(), surface_size, |canvas| {
                // Clear canvas with background color.
                canvas.clear(self.config.colors.background.as_color4f());

                // Prepare visible entries for rendering.
                let entries = self.render_cache.render_entries(
                    &mut self.entries,
                    self.scroll_offset,
                    &grid,
                    self.configuring,
                );

                // Render all entries to the Skia canvas.
                for entry in entries {
                    Self::draw_entry(canvas, &mut self.render_cache, grid.entry_size, entry);
                }
            });
        });

        // Request a new frame.
        wl_surface.frame(&self.queue, wl_surface.clone());

        // Apply surface changes.
        wl_surface.commit();
    }

    /// Draw a prepared desktop entry to the canvas.
    fn draw_entry(
        canvas: &SkiaCanvas,
        render_cache: &mut RenderCache,
        entry_size: Size,
        render_entry: RenderEntry,
    ) {
        // Draw desktop entry name as label.
        if !render_entry.name.is_empty() {
            let mut builder =
                ParagraphBuilder::new(&render_cache.paragraph_style, &render_cache.font_collection);
            builder.add_text(&*render_entry.name);
            let mut paragraph = builder.build();
            paragraph.layout(entry_size.width as f32);
            let y = render_entry.origin.y + entry_size.height as f32 - paragraph.height();
            paragraph.paint(canvas, Point::new(render_entry.origin.x, y));
        }

        // Render icon or cached entry.
        let rect = Rect::new(
            render_entry.image_point.x,
            render_entry.image_point.y,
            render_entry.image_point.x + render_entry.image_size.width as f32,
            render_entry.image_point.y + render_entry.image_size.height as f32,
        );
        canvas.draw_image_rect(&render_entry.image, None, rect, &render_cache.png_paint);

        // Cache new entries after rendering.
        if !render_entry.cached {
            // Convert dimensions for the cache snapshot.
            let left = render_entry.origin.x as i32;
            let top = render_entry.origin.y as i32;
            let right = left + entry_size.width as i32;
            let bottom = top + entry_size.height as i32;
            let entry_rect = IRect::new(left, top, right, bottom);

            // Create a snapshot of the icon and text label.
            let surface = unsafe { canvas.surface() };
            match surface.and_then(|mut s| s.image_snapshot_with_bounds(entry_rect)) {
                Some(snapshot) => {
                    let cache_key = CacheKey { icon: render_entry.icon, name: render_entry.name };
                    render_cache.rendered_entries.insert(cache_key, snapshot);
                },
                None => error!("Failed to create desktop entry snapshot"),
            }
        }
    }

    /// Unstall the renderer.
    ///
    /// This will render a new frame if there currently is no frame request
    /// pending.
    pub fn unstall(&mut self) {
        // Ignore if unstalled or request came from background engine.
        if !mem::take(&mut self.stalled) {
            return;
        }

        // Redraw immediately to unstall rendering.
        self.draw();
        let _ = self.connection.flush();
    }

    /// Update the window's logical size.
    pub fn set_size(&mut self, compositor: &CompositorState, size: Size) {
        if self.size == size && self.initial_draw_done {
            return;
        }

        self.size = size;
        self.dirty = true;

        // Update the window's opaque region.
        //
        // This is done here since it can only change on resize, but the commit happens
        // atomically on redraw.
        if let Ok(region) = Region::new(compositor) {
            region.add(0, 0, size.width as i32, size.height as i32);
            self.xdg_window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        self.unstall();
    }

    /// Update the window's DPI factor.
    pub fn set_scale_factor(&mut self, scale: f64) {
        if self.scale == scale {
            return;
        }

        self.render_cache.set_scale_factor(self.config.font.size, scale);

        self.scale = scale;
        self.dirty = true;

        if self.initial_draw_done {
            self.unstall();
        }
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: Config) {
        let dirty = self.render_cache.update_config(self.scale, &config);

        self.config = config;
        self.dirty |= dirty;

        if dirty {
            self.unstall();
        }
    }

    /// Handle touch press.
    pub fn touch_down(&mut self, logical_point: Point<f64>) {
        // Cancel velocity when a new touch sequence starts.
        self.velocity.set(0.);

        // Convert position to physical space.
        let point = logical_point * self.scale;
        self.touch_state.point = point;
        self.touch_state.start = point;

        if let Some(index) = self.entry_at(point) {
            self.touch_state.action = TouchAction::Tap(index);
        } else {
            self.touch_state.action = TouchAction::None;
        }
    }

    /// Handle touch motion.
    pub fn touch_motion(&mut self, logical_point: Point<f64>) {
        // Update touch position.
        let point = logical_point * self.scale;
        let old_point = mem::replace(&mut self.touch_state.point, point);

        // Ignore dragging until tap distance limit is exceeded.
        let max_tap_distance = self.config.input.max_tap_distance;
        let delta = self.touch_state.point - self.touch_state.start;
        if delta.x.powi(2) + delta.y.powi(2) <= max_tap_distance {
            return;
        }
        self.touch_state.action = TouchAction::Drag;

        // Calculate current scroll velocity.
        let delta = self.touch_state.point.y - old_point.y;
        self.velocity.set(delta);

        // Immediately start moving the tabs list.
        let old_offset = self.scroll_offset;
        self.scroll_offset += delta;
        self.clamp_scroll_offset();
        self.dirty |= self.scroll_offset != old_offset;

        if self.dirty {
            self.unstall();
        }
    }

    /// Handle touch release.
    pub fn touch_up(&mut self) {
        let index = match &self.touch_state.action {
            TouchAction::Tap(index) => index,
            _ => return,
        };
        let entry = if self.configuring {
            self.entries.all_get(*index)
        } else {
            self.entries.visible().nth(*index)
        };
        let entry = match entry {
            Some(entry) => entry,
            None => return,
        };

        match &entry.exec {
            ExecAction::Poweroff if self.configuring => (),
            ExecAction::Poweroff => {
                if let Err(err) = dbus::shutdown() {
                    error!("Shutdown failed: {err}");
                }
            },
            ExecAction::Config => {
                self.configuring = !self.configuring;
                self.dirty = true;
                self.unstall();
            },
            ExecAction::Reboot if self.configuring => (),
            ExecAction::Reboot => {
                if let Err(err) = dbus::reboot() {
                    error!("Reboot failed: {err}");
                }
            },
            ExecAction::Run(_) if self.configuring => {
                // Toggle status of the desktop entry.
                let entry = self.entries.all_get_mut(*index).unwrap();
                if let Err(err) = entry.toggle_hidden() {
                    error!("Failed to toggle hidden status for {:?}: {err}", entry.name);

                    // Remove hidden entries that cannot be toggled.
                    self.entries.remove(*index);
                }

                self.dirty = true;
                self.unstall();
            },
            ExecAction::Run(exec) => {
                let cmd = exec.split(' ').collect::<Vec<_>>();
                match Command::new(cmd[0]).args(&cmd[1..]).spawn() {
                    Ok(_) => process::exit(0),
                    Err(err) => error!("Process launch failed: {err}"),
                }
            },
        }
    }

    /// Get application at the specified location.
    fn entry_at(&self, mut point: Point<f64>) -> Option<usize> {
        point.y -= self.scroll_offset;
        Grid::new(self.size * self.scale, self.scale).index_at(point)
    }

    /// Clamp alarm list viewport offset.
    fn clamp_scroll_offset(&mut self) {
        let old_offset = self.scroll_offset;
        let max_offset = -self.max_scroll_offset();
        self.scroll_offset = self.scroll_offset.clamp(max_offset, 0.);

        // Cancel velocity after reaching the scroll limit.
        if old_offset != self.scroll_offset {
            self.velocity.set(0.);
        }
    }

    /// Get maximum alarm list viewport offset.
    fn max_scroll_offset(&self) -> f64 {
        let entry_count =
            if self.configuring { self.entries.all_len() } else { self.entries.visible().count() };

        let size = self.size * self.scale;
        let grid = Grid::new(size, self.scale);
        let total_height = grid.total_height(entry_count);

        (total_height - size.height as f64).max(0.)
    }
}

/// Skia rendering cache data.
struct RenderCache {
    font_collection: FontCollection,
    paragraph_style: ParagraphStyle,
    text_style: TextStyle,
    font_family: String,
    text_paint: Paint,
    png_paint: Paint,

    rendered_entries: HashMap<CacheKey, Image>,
}

impl RenderCache {
    fn new(config: &Config) -> Self {
        let font_family = config.font.family.clone();

        let mut text_paint = Paint::default();
        text_paint.set_color4f(config.colors.foreground.as_color4f(), None);
        text_paint.set_anti_alias(true);

        let mut text_style = TextStyle::new();
        text_style.set_foreground_paint(&text_paint);
        text_style.set_font_size(config.font.size as f32);
        text_style.set_font_families(&[&font_family]);

        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_align(TextAlign::Center);
        paragraph_style.set_text_style(&text_style);
        paragraph_style.set_ellipsis("…");

        let mut font_collection = FontCollection::new();
        font_collection.set_default_font_manager(FontMgr::new(), None);

        let png_paint = Paint::default();

        Self {
            font_collection,
            paragraph_style,
            font_family,
            text_paint,
            text_style,
            png_paint,
            rendered_entries: Default::default(),
        }
    }

    /// Get render items for desktop entries.
    fn render_entries(
        &self,
        desktop_entries: &mut DesktopEntries,
        scroll_offset: f64,
        grid: &Grid,
        configuring: bool,
    ) -> Vec<RenderEntry> {
        // Update grid indices for rendering.
        let mut index = 0;
        for entry in desktop_entries.all_mut() {
            if configuring || !entry.hidden() {
                entry.grid_index = Some(index);
                index += 1;
            } else {
                entry.grid_index = None;
            }
        }

        // Prepare Skia image(s) for each entry in parallel.
        let rendered_entries = &self.rendered_entries;
        let entries = desktop_entries.all().par_iter().filter_map(|entry| {
            let mut origin = grid.origin(entry.grid_index?);
            origin.y += scroll_offset as f32;

            // Skip invisible entries.
            if origin.y <= -(grid.entry_size.height as f32) || origin.y >= grid.size.height as f32 {
                return None;
            }

            Self::render_entry(rendered_entries, desktop_entries, grid, configuring, entry, origin)
        });
        entries.collect()
    }

    /// Prepare a desktop entry for rendering.
    fn render_entry(
        rendered_entries: &HashMap<CacheKey, Image>,
        desktop_entries: &DesktopEntries,
        grid: &Grid,
        configuring: bool,
        entry: &DesktopEntry,
        origin: Point<f32>,
    ) -> Option<RenderEntry> {
        // Get entry name, picking the config label dynamically.
        let name = if entry.exec != ExecAction::Config || configuring {
            entry.name.clone()
        } else {
            Arc::new(String::new())
        };

        // Load image from cache if available.
        let icon_size = (ICON_SIZE * grid.scale).round() as f32;
        let icon = desktop_entries.icon(entry, icon_size as u32);
        let cache_key = CacheKey { icon, name };
        if let Some(cached) = rendered_entries.get(&cache_key) {
            return Some(RenderEntry {
                origin,
                image_size: grid.entry_size,
                image: cached.clone(),
                icon: cache_key.icon,
                image_point: origin,
                cached: true,
                name: Default::default(),
            });
        }
        let CacheKey { icon, name } = cache_key;

        // Calculate icon position.
        let icon_padding = (grid.entry_size.width - icon_size as u32) as f32 / 2.;
        let icon_point = origin + Point::new(icon_padding, icon_padding);

        // Draw desktop entry icon.
        match icon.icon_type() {
            IconType::Svg => Self::render_svg(origin, name, icon, icon_point, icon_size),
            IconType::Png => Self::render_png(origin, name, icon, icon_point, icon_size),
        }
    }

    /// Render an SVG icon.
    fn render_svg(
        origin: Point<f32>,
        name: Arc<String>,
        icon: Icon,
        icon_point: Point<f32>,
        icon_size: f32,
    ) -> Option<RenderEntry> {
        // Parse SVG data.
        let svg_tree = match SvgTree::from_data(&icon.load(), &SvgOptions::default()) {
            Ok(svg_tree) => svg_tree,
            Err(err) => {
                error!("Failed to parse SVG {name}: {err}");
                return None;
            },
        };

        // Calculate transforms to center SVG inside target buffer.
        let tree_size = svg_tree.size();
        let svg_width = tree_size.width();
        let svg_height = tree_size.height();
        let (svg_scale, x_padding, y_padding) = if svg_width > svg_height {
            (icon_size / svg_width, 0., (svg_width - svg_height) / 2.)
        } else {
            (icon_size / svg_height, (svg_height - svg_width) / 2., 0.)
        };
        let transform =
            SvgTransform::from_translate(x_padding, y_padding).post_scale(svg_scale, svg_scale);

        // Render SVG into CPU buffer.
        let mut pixmap = SvgPixmap::new(icon_size as u32, icon_size as u32).unwrap();
        resvg::render(&svg_tree, transform, &mut pixmap.as_mut());
        let data = Data::new_copy(pixmap.data());

        // Draw SVG buffer to the surface.
        let image_size = Size::new(icon_size as u32, icon_size as u32);
        let info = ImageInfo::new(image_size, ColorType::RGBA8888, AlphaType::Unpremul, None);
        let image = images::raster_from_data(&info, data, icon_size as usize * 4).unwrap();

        Some(RenderEntry {
            image_size,
            origin,
            image,
            icon,
            name,
            image_point: icon_point,
            cached: false,
        })
    }

    /// Render a PNG icon.
    fn render_png(
        origin: Point<f32>,
        name: Arc<String>,
        icon: Icon,
        mut icon_point: Point<f32>,
        icon_size: f32,
    ) -> Option<RenderEntry> {
        // Decode PNG image.
        let image = match Image::from_encoded(Data::new_copy(&icon.load())) {
            Some(image) => image,
            None => {
                error!("Failed to render image for {}", name);
                return None;
            },
        };

        // Ensure PNG aspect ratio is preserved.
        let ratio = image.width() as f32 / image.height() as f32;
        let (width, height) = if ratio > 1. {
            icon_point.y += (icon_size - icon_size / ratio) / 2.;
            (icon_size, icon_size / ratio)
        } else {
            icon_point.x += (icon_size - icon_size * ratio) / 2.;
            (icon_size * ratio, icon_size)
        };

        // Render the image to the canvas.
        let image_size = Size::new(width as u32, height as u32);
        let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear);
        let image_info = ImageInfo::new(image_size, ColorType::RGBA8888, AlphaType::Unpremul, None);
        let image = image.make_scaled(&image_info, sampling).unwrap();

        Some(RenderEntry {
            image_size,
            origin,
            image,
            icon,
            name,
            image_point: icon_point,
            cached: false,
        })
    }

    /// Handle config updates.
    ///
    /// Returns `true` if the update changed the cache.
    fn update_config(&mut self, scale: f64, config: &Config) -> bool {
        let mut dirty = false;

        let foreground = config.colors.foreground.as_color4f();
        if self.text_paint.color4f() != foreground {
            self.text_paint.set_color4f(foreground, None);
            self.text_style.set_foreground_paint(&self.text_paint);
            dirty = true;
        }

        if self.text_style.font_size() != config.font.size as f32 {
            self.text_style.set_font_size((config.font.size * scale) as f32);
            dirty = true;
        }

        if self.font_family != config.font.family {
            self.font_family = config.font.family.clone();
            self.text_style.set_font_families(&[&self.font_family]);
            dirty = true;
        }

        if dirty {
            self.paragraph_style.set_text_style(&self.text_style);
        }

        dirty
    }

    /// Update render scale.
    fn set_scale_factor(&mut self, font_size: f64, scale: f64) {
        // Clear texture cache to redraw icons.
        self.rendered_entries.clear();

        self.text_style.set_font_size((font_size * scale) as f32);
        self.paragraph_style.set_text_style(&self.text_style);
    }
}

/// Data necessary to render a desktop entry.
struct RenderEntry {
    origin: Point<f32>,
    name: Arc<String>,

    image_point: Point<f32>,
    image_size: Size,
    image: Image,

    icon: Icon,

    cached: bool,
}

/// Cache key for the desktop entry render cache.
#[derive(Hash, PartialEq, Eq)]
struct CacheKey {
    name: Arc<String>,
    icon: Icon,
}

/// Touch event tracking.
#[derive(Default)]
struct TouchState {
    action: TouchAction,
    start: Point<f64>,
    point: Point<f64>,
}

/// Intention of a touch sequence.
#[derive(Default)]
enum TouchAction {
    #[default]
    None,
    Tap(usize),
    Drag,
}

/// Grid for entry render positioning.
struct Grid {
    entry_size: Size,
    padding: u32,
    columns: u32,

    scale: f64,
    size: Size,
}

impl Grid {
    fn new(size: Size, scale: f64) -> Self {
        let min_padding = (MIN_PADDING * scale).round() as u32;
        let entry_size = Size::new(ENTRY_WIDTH, ENTRY_HEIGHT) * scale;

        let columns = (size.width - min_padding) / (entry_size.width + min_padding);
        let padding = (size.width - columns * entry_size.width) / (columns + 1);

        Self { entry_size, columns, padding, scale, size }
    }

    /// Get origin point for entry at the specified index.
    fn origin(&self, index: usize) -> Point<f32> {
        match index {
            // Poweroff item position.
            0 => Point::new(self.padding as f32, self.padding as f32),
            // Config item position.
            1 => {
                let x = (self.size.width - self.entry_size.width) as f32 / 2.;
                Point::new(x, self.padding as f32)
            },
            // Reboot item position.
            2 => {
                let x = (self.size.width - self.padding - self.entry_size.width) as f32;
                Point::new(x, self.padding as f32)
            },
            // Desktop entry item position.
            index => {
                let index = index - 3;

                let column = index as u32 % self.columns;
                let row = index as u32 / self.columns + 1;

                let y = (self.entry_size.height + self.padding) * row + self.padding;
                let x = (self.entry_size.width + self.padding) * column + self.padding;

                Point::new(x as f32, y as f32)
            },
        }
    }

    /// Get entry index at the specified position.
    fn index_at(&self, point: Point<f64>) -> Option<usize> {
        // Get position relative to the first entry.
        let x = (point.x.round() as u32).checked_sub(self.padding)?;
        let y = (point.y.round() as u32).checked_sub(self.padding)?;

        // Calculate column in row in a linear grid.
        let column = x / (self.entry_size.width + self.padding);
        let row = y / (self.entry_size.height + self.padding);

        // Handle config entry.
        if row == 0 && column != 0 && column != self.columns - 1 {
            if (point.x as u32) >= (self.size.width - self.entry_size.width) / 2
                && (point.x as u32) < (self.size.width + self.entry_size.width) / 2
                && y < self.entry_size.height
            {
                return Some(1);
            } else {
                return None;
            }
        }

        // Get position relative to the target entry.
        let relative_x = x % (self.entry_size.width + self.padding);
        let relative_y = y % (self.entry_size.height + self.padding);

        // Ignore positions within the padding.
        if relative_x >= self.entry_size.width || relative_y >= self.entry_size.height {
            return None;
        }

        // Account for builtin entries.
        let index = if row == 0 {
            if column == 0 { 0 } else { 2 }
        } else {
            (row - 1) * self.columns + column + 3
        };

        Some(index as usize)
    }

    /// Total height of the grid with the specified number of elements.
    fn total_height(&self, entry_count: usize) -> f64 {
        let rows = (entry_count.saturating_sub(1) as u32 / self.columns) + 1;
        let height = (self.entry_size.height + self.padding) * rows + self.padding;
        height as f64
    }
}
