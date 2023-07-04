//! SVG rasterization.

use std::path::Path;
use std::{fs, io};

use tiny_skia::{Pixmap, Transform};
use usvg::{FitTo, Options, Tree};

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
        let mut options = Options::default();
        options.fontdb.load_system_fonts();

        let tree = Tree::from_data(buffer, &options.to_ref())?;

        let mut pixmap = Pixmap::new(size, size).ok_or(Error::InvalidSize)?;

        let size = FitTo::Size(size, size);
        resvg::render(&tree, size, Transform::default(), pixmap.as_mut())
            .ok_or(Error::InvalidSize)?;

        let width = pixmap.width() as usize;
        let data = pixmap.take();

        Ok(Self { data, width })
    }
}
