//! SVG rasterization.

use core::fmt;
use std::path::Path;
use std::{fs, io};

use resvg::tiny_skia::Pixmap;
use resvg::usvg::{self, Options, Transform, Tree};

/// SVG loading error.
#[derive(Debug)]
pub enum Error {
    Svg(usvg::Error),
    Io(io::Error),
    InvalidSize,
}

impl From<usvg::Error> for Error {
    fn from(error: usvg::Error) -> Self {
        Self::Svg(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Parsed SVG, possibly rendered.
pub struct Svg {
    tree: Tree,
    data: Vec<u8>,
    size: u32,
}

impl fmt::Debug for Svg {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Svg").finish()
    }
}

impl Svg {
    /// Parse a SVG from a path, but don’t render it yet.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let file = fs::read(path)?;
        Self::parse(&file)
    }

    /// Parse a SVG from an XML byte buffer, but don’t render it yet.
    pub fn parse(buffer: &[u8]) -> Result<Self, Error> {
        let options = Options::default();
        let tree = Tree::from_data(buffer, &options)?;
        let data = Vec::new();
        let size = 0;
        Ok(Svg { tree, data, size })
    }

    /// Render this SVG at a specific size.
    pub fn render(&mut self, size: u32) -> Result<(&[u8], u32), Error> {
        if self.size != size {
            let tree_size = self.tree.size();
            let scale = (size as f32 / tree_size.width()).min(size as f32 / tree_size.height());
            let transform = Transform::from_scale(scale, scale);

            let mut pixmap = Pixmap::new(size, size).ok_or(Error::InvalidSize)?;
            resvg::render(&self.tree, transform, &mut pixmap.as_mut());

            self.size = pixmap.width();
            self.data = pixmap.take();
        }

        Ok((&self.data, self.size))
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn width(&self) -> usize {
        self.size as usize
    }
}
