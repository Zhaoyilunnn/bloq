use glam::{IVec3, Vec3};
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};
use thiserror::Error;

/// Error returned when parsing a string or [`IVec3`] into a [`Direction`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DirectionParseError {
    /// The input string is not a recognized direction like `+X`, `-Z`, etc.
    #[error("invalid direction string: {0}")]
    InvalidString(String),
    /// The [`IVec3`] is not a unit axis vector.
    #[error("invalid IVec3 for Direction: {0:?}")]
    InvalidVec(IVec3),
}

/// One of the six signed axis directions in 3D.
///
/// Parses from and displays as `+X`, `-X`, `+Y`, `-Y`, `+Z`, `-Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, Default, EnumIter, EnumString)]
#[strum(
    parse_err_ty = DirectionParseError,
    parse_err_fn = direction_parse_error
)]
pub enum Direction {
    /// Positive X direction.
    #[default]
    #[strum(serialize = "+X")]
    XPLUS,
    /// Negative X direction.
    #[strum(serialize = "-X")]
    XMINUS,
    /// Positive Y direction.
    #[strum(serialize = "+Y")]
    YPLUS,
    /// Negative Y direction.
    #[strum(serialize = "-Y")]
    YMINUS,
    /// Positive Z direction.
    #[strum(serialize = "+Z")]
    ZPLUS,
    /// Negative Z direction.
    #[strum(serialize = "-Z")]
    ZMINUS,
}

impl Direction {
    /// Iterates over all six directions.
    pub fn iter() -> impl Iterator<Item = Self> {
        <Self as IntoEnumIterator>::iter()
    }

    /// Returns a stable `0..6` index for this direction.
    pub const fn index(self) -> usize {
        match self {
            Direction::XPLUS => 0,
            Direction::XMINUS => 1,
            Direction::YPLUS => 2,
            Direction::YMINUS => 3,
            Direction::ZPLUS => 4,
            Direction::ZMINUS => 5,
        }
    }

    /// Returns the unit axis vector pointing along this direction.
    pub const fn to_ivec3(self) -> IVec3 {
        match self {
            Direction::XPLUS => IVec3::new(1, 0, 0),
            Direction::XMINUS => IVec3::new(-1, 0, 0),
            Direction::YPLUS => IVec3::new(0, 1, 0),
            Direction::YMINUS => IVec3::new(0, -1, 0),
            Direction::ZPLUS => IVec3::new(0, 0, 1),
            Direction::ZMINUS => IVec3::new(0, 0, -1),
        }
    }

    /// Returns the unit axis vector as a floating-point [`Vec3`].
    pub fn to_vec3(self) -> Vec3 {
        self.to_ivec3().as_vec3()
    }

    /// Returns `true` for the `X`/`Y` (spatial) directions.
    pub const fn is_spatial(self) -> bool {
        matches!(
            self,
            Direction::XPLUS | Direction::XMINUS | Direction::YPLUS | Direction::YMINUS
        )
    }

    /// Returns the opposite direction along the same axis.
    pub const fn negate(self) -> Self {
        match self {
            Direction::XPLUS => Direction::XMINUS,
            Direction::XMINUS => Direction::XPLUS,
            Direction::YPLUS => Direction::YMINUS,
            Direction::YMINUS => Direction::YPLUS,
            Direction::ZPLUS => Direction::ZMINUS,
            Direction::ZMINUS => Direction::ZPLUS,
        }
    }

    /// Drops the sign, returning the unsigned axis [`UDirection`].
    pub const fn as_udirection(self) -> UDirection {
        match self {
            Direction::XPLUS | Direction::XMINUS => UDirection::X,
            Direction::YPLUS | Direction::YMINUS => UDirection::Y,
            Direction::ZPLUS | Direction::ZMINUS => UDirection::Z,
        }
    }
}

impl TryFrom<IVec3> for Direction {
    type Error = DirectionParseError;

    fn try_from(value: IVec3) -> Result<Self, Self::Error> {
        for dir in Direction::iter() {
            if value == dir.to_ivec3() {
                return Ok(dir);
            }
        }
        Err(DirectionParseError::InvalidVec(value))
    }
}

fn direction_parse_error(value: &str) -> DirectionParseError {
    DirectionParseError::InvalidString(value.to_string())
}

/// An unsigned axis (`X`, `Y`, or `Z`): a [`Direction`] with its sign dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, Default, EnumIter, EnumString)]
pub enum UDirection {
    /// X axis.
    #[default]
    X,
    /// Y axis.
    Y,
    /// Z axis.
    Z,
}

impl UDirection {
    /// Iterates over the three unsigned axes.
    pub fn iter() -> impl Iterator<Item = Self> {
        <Self as IntoEnumIterator>::iter()
    }

    /// Returns the positive unit axis vector for this axis.
    pub const fn to_ivec3(self) -> IVec3 {
        match self {
            UDirection::X => IVec3::new(1, 0, 0),
            UDirection::Y => IVec3::new(0, 1, 0),
            UDirection::Z => IVec3::new(0, 0, 1),
        }
    }

    /// Returns the positive unit axis vector as a floating-point [`Vec3`].
    pub fn to_vec3(self) -> Vec3 {
        self.to_ivec3().as_vec3()
    }

    /// Returns `true` for the `X`/`Y` (spatial) axes.
    pub const fn is_spatial(self) -> bool {
        matches!(self, UDirection::X | UDirection::Y)
    }

    /// Returns a stable `0..3` index for this axis.
    pub const fn index(self) -> usize {
        match self {
            UDirection::X => 0,
            UDirection::Y => 1,
            UDirection::Z => 2,
        }
    }
}
