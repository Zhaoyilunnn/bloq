//! Cube height: a symbolic linear function of the code distance.
//!
//! Writing a cube's height as `k*d + n` splits two things a plain integer
//! multiplier conflated — **cells**, the lattice cells it occupies along the
//! time axis, and **rounds**, the syndrome rounds it compiles to — so a cube can
//! ask for fewer rounds than its footprint suggests. `height=2d/3` occupies one cell
//! but compiles to `ceil(2d/3)` rounds, which is what makes TELS expressible: a
//! CCZ factory's Z-readout layers carry a second distance-2 discard redundancy
//! and so do not need the full `d` rounds a bare merge does.

use std::fmt;
use std::str::FromStr;

use crate::BlockError;

/// Largest supported cube footprint along the time axis, in cells.
///
/// A cube eagerly materializes one reserved offset per occupied cell, so this
/// limit bounds allocations from both parsed and programmatic graph input. It
/// also keeps [`CubeHeight::rounds`] inside `i64`: rounds are at most
/// `cells * distance`, and `65535 * u32::MAX` is comfortably representable.
pub const MAX_CUBE_HEIGHT_CELLS: u32 = 65_535;

/// A cube's height, written `k*d + n` over the code distance `d`.
///
/// `k = numerator / denominator` is a positive rational kept in lowest terms so
/// that `2d/4 == d/2`; `n` is a signed integer offset. Blog syntax is
/// `height=<expr>` — `d`, `2d`, `d/2`, `3d/2`, `3d+2`, `d/2 - 1` — defaulting to
/// `height=d`. [`Ord`] is structural, present only to give [`Block`](crate::Block) a
/// total order; it does not compare realized heights, which depend on `d`.
///
/// ```
/// use bloq_graph::CubeHeight;
///
/// let half: CubeHeight = "d/2".parse()?;
/// assert_eq!((half.cells(), half.rounds(7)), (1, 4)); // one cell, ceil(7/2) rounds
/// # Ok::<(), bloq_graph::BlockError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CubeHeight {
    numerator: u32,
    denominator: u32,
    offset: i32,
}

impl CubeHeight {
    /// `height=d`: one code distance of rounds, one cell.
    pub const DEFAULT: Self = Self {
        numerator: 1,
        denominator: 1,
        offset: 0,
    };

    /// Constructs `numerator/denominator * d + offset`, reduced to lowest terms.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::NonPositiveCubeHeight`] if either `numerator` or
    /// `denominator` is zero (`k <= 0` is meaningless), or
    /// [`BlockError::CubeHeightExceedsRange`] if the footprint would exceed
    /// [`MAX_CUBE_HEIGHT_CELLS`].
    pub fn new(numerator: u32, denominator: u32, offset: i32) -> Result<Self, BlockError> {
        if numerator == 0 || denominator == 0 {
            return Err(BlockError::NonPositiveCubeHeight {
                numerator,
                denominator,
            });
        }
        let divisor = gcd(numerator, denominator);
        let height = Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
            offset,
        };
        if height.cells() > MAX_CUBE_HEIGHT_CELLS {
            return Err(BlockError::CubeHeightExceedsRange {
                cells: height.cells(),
            });
        }
        Ok(height)
    }

    /// The reduced numerator of `k`.
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// The reduced denominator of `k`.
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// The integer offset `n`.
    pub const fn offset(self) -> i32 {
        self.offset
    }

    /// Whether this is `height=d`, the height a cube has when nothing is written.
    pub const fn is_default(self) -> bool {
        self.numerator == 1 && self.denominator == 1 && self.offset == 0
    }

    /// Cells occupied along the time axis: `ceil(k)`, always at least 1.
    ///
    /// The offset `n` deliberately does not participate. `n` buys or refunds
    /// individual syndrome rounds inside the cube's own worldline; it does not
    /// change which lattice cells the cube claims, so `height=d-1` and `height=d+1` are
    /// both single-cell cubes.
    pub const fn cells(self) -> u32 {
        self.numerator.div_ceil(self.denominator)
    }

    /// Compiled syndrome rounds at `distance`: `ceil(k*d) + n`.
    ///
    /// Since `n` is an integer this equals `ceil(k*d + n)` exactly, and it is
    /// computed that way — integer `div_ceil` on the numerator, then the offset
    /// — rather than through floating point.
    ///
    /// The result is signed because a large negative `n` can drive it below
    /// zero; callers that need a usable round count must check it (a cube needs
    /// at least 2 rounds, one for its initialization stage and one for its
    /// measurement stage).
    pub const fn rounds(self, distance: u32) -> i64 {
        let scaled = (self.numerator as u64 * distance as u64).div_ceil(self.denominator as u64);
        // `scaled <= cells() * distance <= 65535 * u32::MAX`, so the cast is
        // lossless and the sum cannot overflow `i64`.
        scaled as i64 + self.offset as i64
    }
}

/// Greatest common divisor, for reducing `k` at construction.
const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

impl Default for CubeHeight {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for CubeHeight {
    /// Emits the canonical form: the fraction reduced, a `/1` denominator and a
    /// `+0` offset omitted. This is exactly what the blog writer puts after
    /// `height=`, so it must re-parse to an equal value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.numerator != 1 {
            write!(f, "{}", self.numerator)?;
        }
        f.write_str("d")?;
        if self.denominator != 1 {
            write!(f, "/{}", self.denominator)?;
        }
        match self.offset {
            0 => Ok(()),
            n if n > 0 => write!(f, "+{n}"),
            n => write!(f, "-{}", n.unsigned_abs()),
        }
    }
}

impl FromStr for CubeHeight {
    type Err = BlockError;

    /// Parses `k*d + n`, sharing the blog grammar so the two cannot drift.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::parser::cube_height_from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_spellings_parse_to_the_documented_value() {
        for (text, (numerator, denominator, offset)) in [
            ("d", (1, 1, 0)),
            ("2d", (2, 1, 0)),
            ("3d", (3, 1, 0)),
            ("d/2", (1, 2, 0)),
            ("3d/2", (3, 2, 0)),
            ("2d/3", (2, 3, 0)),
            ("3d+2", (3, 1, 2)),
            ("d/2 + 1", (1, 2, 1)),
            ("d - 1", (1, 1, -1)),
            (" 2 d / 4 ", (1, 2, 0)),
            ("D/2", (1, 2, 0)),
        ] {
            let height: CubeHeight = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(
                (height.numerator(), height.denominator(), height.offset()),
                (numerator, denominator, offset),
                "parsing {text}"
            );
        }
    }

    #[test]
    fn non_positive_and_malformed_expressions_are_rejected() {
        for text in [
            "0d",
            "d/0",
            "0d/0",
            "2",
            "",
            "dd",
            "d/",
            "d+",
            "d/2/3",
            "d*2",
            "d+2147483648",
            "d-2147483649",
        ] {
            assert!(
                text.parse::<CubeHeight>().is_err(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn display_is_canonical_and_round_trips() {
        for (input, canonical) in [
            ("d", "d"),
            ("1d/1", "d"),
            ("2d/4", "d/2"),
            ("2d", "2d"),
            ("3d/2", "3d/2"),
            ("3d + 2", "3d+2"),
            ("d/2 - 1", "d/2-1"),
            ("d+0", "d"),
            ("d+2147483647", "d+2147483647"),
            ("d-2147483648", "d-2147483648"),
        ] {
            let height: CubeHeight = input.parse().expect("valid height");
            assert_eq!(height.to_string(), canonical);
            assert_eq!(
                canonical
                    .parse::<CubeHeight>()
                    .expect("canonical re-parses"),
                height
            );
        }
    }

    #[test]
    fn cells_is_ceil_k_and_ignores_the_offset() {
        for (text, cells) in [
            ("d", 1),
            ("d/2", 1),
            ("2d/3", 1),
            ("2d", 2),
            ("3d/2", 2),
            ("3d", 3),
            ("d-1", 1),
            ("d+5", 1),
        ] {
            assert_eq!(
                text.parse::<CubeHeight>().expect("valid").cells(),
                cells,
                "cells of {text}"
            );
        }
    }

    #[test]
    fn rounds_is_ceil_kd_plus_the_offset() {
        for (text, distance, rounds) in [
            ("d", 7, 7),
            ("2d", 7, 14),
            ("d/2", 7, 4),
            ("d/2", 8, 4),
            ("3d+2", 7, 23),
            ("d/2 + 1", 7, 5),
            ("2d/3", 7, 5),
            // A negative offset can drive the count below the two-round floor,
            // and even below zero; the compiler is what rejects that.
            ("d-1", 3, 2),
            ("d/2-3", 3, -1),
        ] {
            assert_eq!(
                text.parse::<CubeHeight>().expect("valid").rounds(distance),
                rounds,
                "rounds of {text} at d={distance}"
            );
        }
    }

    #[test]
    fn footprint_is_bounded_before_reserved_offset_allocation() {
        CubeHeight::new(MAX_CUBE_HEIGHT_CELLS, 1, 0).unwrap();
        assert!(matches!(
            CubeHeight::new(MAX_CUBE_HEIGHT_CELLS + 1, 1, 0),
            Err(BlockError::CubeHeightExceedsRange { cells }) if cells == MAX_CUBE_HEIGHT_CELLS + 1
        ));
        // A huge numerator is fine as long as the reduced ceiling is small.
        CubeHeight::new(u32::MAX, u32::MAX, 0).unwrap();
    }
}
