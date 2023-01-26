//! Text rendering.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};

use crossfont::ft::FreeTypeRasterizer;
use crossfont::{
    BitmapBuffer, Error, Error as RasterizerError, FontDesc, FontKey, GlyphKey, Metrics, Rasterize,
    RasterizedGlyph, Size, Slant, Style, Weight,
};

use crate::renderer::TextureBuffer;

/// Text rasterizer.
#[derive(Debug)]
pub struct Rasterizer {
    cache: HashMap<char, RasterizedGlyph>,
    ellipsis_width: usize,
    ft: FontRasterizer,
    font_name: String,
}

impl Rasterizer {
    /// Create a new text rasterizer.
    pub fn new(
        font_name: impl Into<String>,
        size: impl Into<Size>,
        scale_factor: i32,
    ) -> Result<Self, Error> {
        let font_name = font_name.into();
        let size = size.into();

        // Initialize freetype rasterizer.
        let mut rasterizer = FreeTypeRasterizer::new(1.)?;

        // Load specified font.
        let font = Self::load_font(&mut rasterizer, &font_name, size, scale_factor)?;

        let mut rasterizer = Self {
            font_name,
            ft: FontRasterizer { scale_factor, rasterizer, font, size },
            cache: HashMap::new(),
            ellipsis_width: 0,
        };

        // Initialize metrics and store ellipsis' width.
        rasterizer.initialize_metrics()?;

        Ok(rasterizer)
    }

    /// Rasterize a string into an OpenGL texture.
    pub fn rasterize(
        &mut self,
        buffer: &mut TextureBuffer,
        center: (usize, usize),
        text: &str,
        max_width: usize,
    ) -> Result<(), Error> {
        // Ensure all rasterized glyphs are cached.
        for character in text.chars() {
            if let Entry::Vacant(entry) = self.cache.entry(character) {
                let glyph = match self.ft.get_glyph(character) {
                    Ok(glyph) => glyph,
                    Err(RasterizerError::MissingGlyph(rasterized)) => rasterized,
                    Err(err) => return Err(err),
                };
                entry.insert(glyph);
            }
        }

        let mut glyphs = Vec::new();
        let mut width = 0;
        for character in text.chars() {
            let glyph = self.cache.get(&character).unwrap();
            let advance = glyph.advance.0 as usize;

            // Truncate text that is too long and add an ellipsis.
            if width + advance + self.ellipsis_width > max_width {
                glyphs.push(self.cache.get(&'…').unwrap());
                width += self.ellipsis_width;
                break;
            } else {
                glyphs.push(glyph);
                width += advance;
            }
        }

        let metrics = self.ft.metrics()?;
        let height = metrics.line_height as usize;
        let ascent = height - (-metrics.descent) as usize;

        let anchor_x = center.0.saturating_sub(width / 2);
        let anchor_y = center.1;

        let mut offset = 0;

        let mut glyphs_iter = glyphs.iter().peekable();
        while let Some(glyph) = glyphs_iter.next() {
            let copy_fun: fn(&mut TextureBuffer, &[u8], usize, (usize, usize));
            let (stride, glyph_buffer) = match &glyph.buffer {
                BitmapBuffer::Rgb(glyph_buffer) => {
                    copy_fun = TextureBuffer::write_rgb_at;
                    (3, glyph_buffer)
                },
                BitmapBuffer::Rgba(glyph_buffer) => {
                    copy_fun = TextureBuffer::write_rgba_at;
                    (4, glyph_buffer)
                },
            };

            if !glyph_buffer.is_empty() {
                // Glyph position inside the buffer.
                let y = anchor_y + ascent - glyph.top as usize;
                let x = ((anchor_x + offset) as i32 + glyph.left) as usize;

                // Copy the rasterized glyph to the output buffer.
                let row_width = glyph.width as usize * stride;
                copy_fun(buffer, glyph_buffer, row_width, (x, y));
            }

            // Get glyph kerning offsets.
            let next = glyphs_iter.peek().map(|next| next.character).unwrap_or_default();
            let kerning = self.ft.kerning(glyph.character, next);

            // Advance write position by glyph width.
            offset += (glyph.advance.0 + kerning.0 as i32) as usize;
        }

        Ok(())
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: i32) {
        // Avoid clearing all caches when factor didn't change.
        if self.ft.scale_factor == scale_factor {
            return;
        }
        self.ft.scale_factor = scale_factor;

        // Clear glyph cache.
        self.cache.clear();

        // Load font at new size.
        self.ft.font =
            Self::load_font(&mut self.ft.rasterizer, &self.font_name, self.ft.size, scale_factor)
                .unwrap_or(self.ft.font);
        let _ = self.initialize_metrics();
    }

    /// Text height in pixels.
    pub fn line_height(&self) -> usize {
        self.ft.metrics().map_or(0, |metrics| metrics.line_height as usize)
    }

    /// Load a new font.
    fn load_font(
        rasterizer: &mut FreeTypeRasterizer,
        font_name: &str,
        size: Size,
        scale_factor: i32,
    ) -> Result<FontKey, Error> {
        let font_style = Style::Description { slant: Slant::Normal, weight: Weight::Normal };
        let font_desc = FontDesc::new(font_name, font_style);
        rasterizer.load_font(&font_desc, size * scale_factor as f32)
    }

    /// Initialize font metrics, updating the ellipsis' width.
    fn initialize_metrics(&mut self) -> Result<(), Error> {
        let ellipsis = self.ft.get_glyph('…')?;
        self.ellipsis_width = ellipsis.advance.0 as usize;
        self.cache.insert('…', ellipsis);
        Ok(())
    }
}

struct FontRasterizer {
    rasterizer: FreeTypeRasterizer,
    scale_factor: i32,
    font: FontKey,
    size: Size,
}

impl FontRasterizer {
    /// Get font metrics.
    fn metrics(&self) -> Result<Metrics, Error> {
        self.rasterizer.metrics(self.font, self.font_size())
    }

    /// Get glyph kerning.
    fn kerning(&mut self, left: char, right: char) -> (f32, f32) {
        self.rasterizer.kerning(self.glyph_key(left), self.glyph_key(right))
    }

    /// Rasterize a character.
    fn get_glyph(&mut self, character: char) -> Result<RasterizedGlyph, Error> {
        self.rasterizer.get_glyph(self.glyph_key(character))
    }

    /// Get glyph key for a character.
    fn glyph_key(&self, character: char) -> GlyphKey {
        GlyphKey { font_key: self.font, size: self.font_size(), character }
    }

    /// Scaled font size.
    fn font_size(&self) -> Size {
        self.size * self.scale_factor as f32
    }
}

impl Debug for FontRasterizer {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("FreetypeRasterizer")
            .field("font", &self.font)
            .field("size", &self.size)
            .finish()
    }
}
