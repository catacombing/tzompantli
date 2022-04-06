//! Text rendering.

use std::cmp;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};

use crossfont::ft::FreeTypeRasterizer;
use crossfont::{
    BitmapBuffer, Error, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph, Size,
    Slant, Style, Weight,
};

use crate::renderer::Texture;

/// Text rasterizer.
#[derive(Debug)]
pub struct Rasterizer {
    cache: HashMap<char, RasterizedGlyph>,
    ft: FreetypeRasterizer,
}

impl Rasterizer {
    /// Create a new text rasterizer.
    pub fn new(font: &str, size: impl Into<Size>) -> Result<Self, Error> {
        let mut rasterizer = FreeTypeRasterizer::new(1., false)?;
        let size = size.into();

        let font_style = Style::Description { slant: Slant::Normal, weight: Weight::Normal };
        let font_desc = FontDesc::new(font, font_style);
        let font = rasterizer.load_font(&font_desc, size)?;

        let ft = FreetypeRasterizer { font, size, rasterizer };
        Ok(Self { ft, cache: Default::default() })
    }

    /// Rasterize a string into an OpenGL texture.
    pub fn rasterize(&mut self, text: &str) -> Result<Texture, Error> {
        // Ensure all rasterized glyphs are cached.
        for character in text.chars() {
            if let Entry::Vacant(entry) = self.cache.entry(character) {
                let glyph = self.ft.get_glyph(character)?;
                entry.insert(glyph);
            }
        }

        let glyphs: Vec<_> = text.chars().map(|c| self.cache.get(&c).unwrap()).collect();

        let metrics = self.ft.metrics()?;
        let width: usize = glyphs.iter().map(|glyph| glyph.advance.0 as usize).sum();
        let height = metrics.line_height as usize;
        let ascent = height - (-metrics.descent) as usize;

        let mut offset = 0;
        let mut buffer = vec![0; width * height * 4];

        let mut glyphs_iter = glyphs.iter().peekable();
        while let Some(glyph) = glyphs_iter.next() {
            let copy_fun: fn(&[u8], &mut [u8]);
            let (stride, glyph_buffer) = match &glyph.buffer {
                BitmapBuffer::Rgb(buffer) => {
                    copy_fun = copy_rgb;
                    (3, buffer)
                },
                BitmapBuffer::Rgba(buffer) => {
                    copy_fun = copy_rgba;
                    (4, buffer)
                },
            };

            // Cut off glyphs extending beyond the buffer's width.
            let glyph_width = glyph.width as usize;
            let mut row_width = cmp::min(glyph_width, width - offset / 4);

            // Glyph position inside the buffer.
            let y = ascent - glyph.top as usize;
            let x = glyph.left * 4 + offset as i32;

            // Cut off glyphs with negative offset at the start of the buffer.
            let x_offset = cmp::max(-x, 0) as usize / 4 * stride;
            row_width -= x_offset / stride;
            let x = cmp::max(x, 0) as usize;

            // Copy each row in the rasterized glyph to the texture buffer.
            for row in 0..glyph.height as usize {
                let dst_start = (row + y) * width * 4 + x;
                let dst_end = dst_start + row_width * 4;
                let dst = &mut buffer[dst_start..dst_end];

                let src_start = row * glyph_width * stride + x_offset;
                let src_end = src_start + row_width * stride;
                let src = &glyph_buffer[src_start..src_end];

                copy_fun(src, dst);
            }

            // Get glyph kerning offsets.
            let next = glyphs_iter.peek().map(|next| next.character).unwrap_or_default();
            let kerning = self.ft.kerning(glyph.character, next);

            // Advance write position by glyph width.
            offset += (glyph.advance.0 + kerning.0 as i32) as usize * 4;
        }

        Ok(Texture::new(&buffer, width, height))
    }
}

struct FreetypeRasterizer {
    rasterizer: FreeTypeRasterizer,
    font: FontKey,
    size: Size,
}

impl FreetypeRasterizer {
    /// Get font metrics.
    fn metrics(&self) -> Result<Metrics, Error> {
        self.rasterizer.metrics(self.font, self.size)
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
        GlyphKey { font_key: self.font, size: self.size, character }
    }
}

impl Debug for FreetypeRasterizer {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("FreetypeRasterizer")
            .field("font", &self.font)
            .field("size", &self.size)
            .finish()
    }
}

/// Copy an RGB buffer to an RGBA destination.
fn copy_rgb(src: &[u8], dst: &mut [u8]) {
    debug_assert!(src.len() / 3 == dst.len() / 4);

    for (i, chunk) in src.chunks(3).enumerate().filter(|(_i, chunk)| chunk != &[0; 3]) {
        dst[i * 4] = chunk[0];
        dst[i * 4 + 1] = chunk[1];
        dst[i * 4 + 2] = chunk[2];
    }
}

/// Copy an RGBA buffer to an RGBA destination.
fn copy_rgba(src: &[u8], dst: &mut [u8]) {
    for (i, chunk) in src.chunks(4).enumerate().filter(|(_i, chunk)| chunk != &[0; 4]) {
        dst[i * 4] = chunk[0];
        dst[i * 4 + 1] = chunk[1];
        dst[i * 4 + 2] = chunk[2];
        dst[i * 4 + 3] = chunk[3];
    }
}
