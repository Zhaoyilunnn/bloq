use crate::{Basis, Pauli, PauliBasis};

/// An 8-bit-per-channel RGBA color.
///
/// Every channel is `0..=255`; `a` is opacity, with `255` fully opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RGBA {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel.
    pub a: u8,
}

impl RGBA {
    /// Softened red for the Pauli/basis `X` axis.
    pub const X_RED: RGBA = RGBA::from_hex(0xFF7F7F, 255);
    /// Softened green for the Pauli `Y` axis.
    pub const Y_GREEN: RGBA = RGBA::from_hex(0x63C676, 255);
    /// Softened blue for the Pauli/basis `Z` axis.
    pub const Z_BLUE: RGBA = RGBA::from_hex(0x7396FF, 255);
    /// Yellow for Hadamard blocks.
    pub const H_YELLOW: RGBA = RGBA::from_hex(0xFFFF65, 255);
    /// Magenta for T blocks, kept clear of [`Self::Y_GREEN`].
    pub const T_PURPLE: RGBA = RGBA::from_hex(0xFF39C2, 255);
    /// Opaque black for outlines and edges.
    pub const LINE_BLACK: RGBA = RGBA::from_hex(0x000000, 255);
    /// Semi-transparent light gray for unconnected ports.
    pub const PORT_GRAY: RGBA = RGBA::from_hex(0xDDDDDD, 89);
    /// Fully saturated red for the Pauli `X` axis.
    pub const X_PURE_RED: RGBA = RGBA::from_hex(0xFF0000, 255);
    /// Fully saturated green for the Pauli `Y` axis.
    pub const Y_PURE_GREEN: RGBA = RGBA::from_hex(0x00FF00, 255);
    /// Fully saturated blue for the Pauli `Z` axis.
    pub const Z_PURE_BLUE: RGBA = RGBA::from_hex(0x0000FF, 255);

    /// Builds a color from a packed `0xRRGGBB` value and a separate alpha.
    pub const fn from_hex(rgb_hex: u32, alpha: u8) -> Self {
        RGBA {
            r: ((rgb_hex >> 16) & 0xFF) as u8,
            g: ((rgb_hex >> 8) & 0xFF) as u8,
            b: (rgb_hex & 0xFF) as u8,
            a: alpha,
        }
    }

    /// Converts to a normalized `[r, g, b, a]` array with each channel in `0.0..=1.0`.
    pub const fn to_f32_array(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
}

impl From<Basis> for RGBA {
    fn from(basis: Basis) -> Self {
        match basis {
            Basis::X => Self::X_RED,
            Basis::Z => Self::Z_BLUE,
        }
    }
}

impl From<PauliBasis> for RGBA {
    fn from(basis: PauliBasis) -> Self {
        match basis {
            PauliBasis::X => Self::X_RED,
            PauliBasis::Y => Self::Y_GREEN,
            PauliBasis::Z => Self::Z_BLUE,
        }
    }
}

impl From<Pauli> for RGBA {
    fn from(pauli: Pauli) -> Self {
        match pauli {
            Pauli::I => RGBA::LINE_BLACK,
            Pauli::X => Self::X_RED,
            Pauli::Y => Self::Y_GREEN,
            Pauli::Z => Self::Z_BLUE,
        }
    }
}
