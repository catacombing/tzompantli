//! Configuration options.

/// Font configuration.
pub mod font {
    /// Font description.
    pub const FONT: &str = "Sans";

    /// Font size.
    pub const FONT_SIZE: f32 = 12.;
}

/// Color configuration.
pub mod colors {
    /// Primary background color.
    pub const BG: Color = Color { r: 24, g: 24, b: 24 };

    /// RGB color.
    #[derive(Copy, Clone)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
    }

    impl Color {
        pub const fn as_f32(&self) -> [f32; 3] {
            [self.r as f32 / 255., self.g as f32 / 255., self.b as f32 / 255.]
        }
    }
}

/// Input configuration.
pub mod input {
    /// Square of the maximum distance before touch input is considered a drag.
    pub const MAX_TAP_DISTANCE: f64 = 400.;

    /// Speed multiplier when using pointer rather than touch scrolling.
    pub const MOUSEWHEEL_SPEED: f64 = 10.;
}
