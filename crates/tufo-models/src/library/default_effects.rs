//! The effects a library's Effects soundboard starts with.

use jiff::Timestamp;

use super::{BuiltinTitle, SoundIcon};

/// An ambience loop from Blanket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultEffect {
    Rain,
    Storm,
    Wind,
    Waves,
    Stream,
    Birds,
    SummerNight,
    Fireplace,
}

impl DefaultEffect {
    pub const ALL: [Self; 8] = [
        Self::Rain,
        Self::Storm,
        Self::Wind,
        Self::Waves,
        Self::Stream,
        Self::Birds,
        Self::SummerNight,
        Self::Fireplace,
    ];

    /// Also names the effect's file, which the library titles it after.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Rain => "Rain",
            Self::Storm => "Storm",
            Self::Wind => "Wind",
            Self::Waves => "Waves",
            Self::Stream => "Stream",
            Self::Birds => "Birds",
            Self::SummerNight => "Summer Night",
            Self::Fireplace => "Fireplace",
        }
    }

    #[must_use]
    pub fn builtin_title(self) -> BuiltinTitle {
        match self {
            Self::Rain => BuiltinTitle::Rain,
            Self::Storm => BuiltinTitle::Storm,
            Self::Wind => BuiltinTitle::Wind,
            Self::Waves => BuiltinTitle::Waves,
            Self::Stream => BuiltinTitle::Stream,
            Self::Birds => BuiltinTitle::Birds,
            Self::SummerNight => BuiltinTitle::SummerNight,
            Self::Fireplace => BuiltinTitle::Fireplace,
        }
    }

    #[must_use]
    pub fn icon(self) -> SoundIcon {
        match self {
            Self::Rain => SoundIcon::CloudRain,
            Self::Storm => SoundIcon::CloudLightning,
            Self::Wind => SoundIcon::Wind,
            Self::Waves => SoundIcon::Waves,
            Self::Stream => SoundIcon::Droplets,
            Self::Birds => SoundIcon::Bird,
            Self::SummerNight => SoundIcon::MoonStar,
            Self::Fireplace => SoundIcon::FlameKindling,
        }
    }
}

/// The one row of [`DefaultEffectsSeed`].
pub const DEFAULT_EFFECTS_SEED_ID: i64 = 1;

/// When the library got its Effects soundboard; one row at most, so a deleted one stays deleted.
#[derive(Debug, toasty::Model)]
#[table = "default_effects_seed"]
pub struct DefaultEffectsSeed {
    #[key]
    pub id: i64,
    pub seeded_at: Timestamp,
}
