//! SVG rasterization.

use std::path::Path;
use std::{fs, io};

use resvg::tiny_skia::Pixmap;
use resvg::usvg::{self, Options, Transform, Tree, TreeParsing};

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

/// Rendered SVG.
#[derive(Debug)]
pub struct Svg {
    pub data: Vec<u8>,
    pub width: usize,
}

impl Svg {
    /// Render an SVG from a path at a specific size.
    pub fn from_path<P: AsRef<Path>>(path: P, size: u32) -> Result<Self, Error> {
        let file = fs::read(path)?;
        Self::from_buffer(&file, size)
    }

    /// Render an SVG from XML byte buffer at a specific size.
    pub fn from_buffer(buffer: &[u8], size: u32) -> Result<Self, Error> {
        let options = Options::default();
        let tree = Tree::from_data(buffer, &options)?;
        let scale = (size as f32 / tree.size.width()).min(size as f32 / tree.size.height());

        let mut pixmap = Pixmap::new(size, size).ok_or(Error::InvalidSize)?;
        let tree = resvg::Tree::from_usvg(&tree);
        let transform = Transform::from_scale(scale, scale);
        tree.render(transform, &mut pixmap.as_mut());

        let width = pixmap.width() as usize;
        let data = pixmap.take();

        Ok(Self { data, width })
    }
}
