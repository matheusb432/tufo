use tufo_models::library::{BackgroundPreset, SoundIcon};

pub(crate) fn parse_background_preset(text: &str) -> Result<BackgroundPreset, String> {
    BackgroundPreset::ALL
        .into_iter()
        .find(|preset| background_preset(*preset) == text)
        .ok_or_else(|| "unknown background.preset; see `tufo collection add --help`".to_owned())
}

pub(crate) fn parse_icon(text: &str) -> Result<SoundIcon, String> {
    SoundIcon::ALL
        .iter()
        .copied()
        .find(|icon| sound_icon(*icon) == text)
        .ok_or_else(|| "unknown icon; see `tufo entry update --help`".to_owned())
}

pub(crate) fn background_preset(value: BackgroundPreset) -> &'static str {
    match value {
        BackgroundPreset::Autumn => "autumn",
        BackgroundPreset::Battle => "battle",
        BackgroundPreset::Castle => "castle",
        BackgroundPreset::Cathedral => "cathedral",
        BackgroundPreset::Coast => "coast",
        BackgroundPreset::Desert => "desert",
        BackgroundPreset::Dragon => "dragon",
        BackgroundPreset::Fire => "fire",
        BackgroundPreset::Forest => "forest",
        BackgroundPreset::Haunted => "haunted",
        BackgroundPreset::Mountain => "mountain",
        BackgroundPreset::Night => "night",
        BackgroundPreset::Sky => "sky",
        BackgroundPreset::Skulls => "skulls",
        BackgroundPreset::Storm => "storm",
        BackgroundPreset::Tavern => "tavern",
        BackgroundPreset::Town => "town",
        BackgroundPreset::Underworld => "underworld",
        BackgroundPreset::Volcano => "volcano",
        BackgroundPreset::Winter => "winter",
    }
}

pub(crate) fn sound_icon(value: SoundIcon) -> &'static str {
    match value {
        SoundIcon::AudioLines => "audio_lines",
        SoundIcon::Anchor => "anchor",
        SoundIcon::Axe => "axe",
        SoundIcon::Beer => "beer",
        SoundIcon::Bell => "bell",
        SoundIcon::Bird => "bird",
        SoundIcon::Bomb => "bomb",
        SoundIcon::Bone => "bone",
        SoundIcon::BowArrow => "bow_arrow",
        SoundIcon::Bug => "bug",
        SoundIcon::Building => "building",
        SoundIcon::Castle => "castle",
        SoundIcon::Cat => "cat",
        SoundIcon::Church => "church",
        SoundIcon::Clock => "clock",
        SoundIcon::CloudFog => "cloud_fog",
        SoundIcon::CloudLightning => "cloud_lightning",
        SoundIcon::CloudRain => "cloud_rain",
        SoundIcon::CloudSnow => "cloud_snow",
        SoundIcon::Coffee => "coffee",
        SoundIcon::Coins => "coins",
        SoundIcon::Crown => "crown",
        SoundIcon::Dices => "dices",
        SoundIcon::Dog => "dog",
        SoundIcon::DoorOpen => "door_open",
        SoundIcon::Droplets => "droplets",
        SoundIcon::Drum => "drum",
        SoundIcon::Fan => "fan",
        SoundIcon::Fish => "fish",
        SoundIcon::Flame => "flame",
        SoundIcon::FlameKindling => "flame_kindling",
        SoundIcon::FlaskConical => "flask_conical",
        SoundIcon::Footprints => "footprints",
        SoundIcon::Ghost => "ghost",
        SoundIcon::Guitar => "guitar",
        SoundIcon::Hammer => "hammer",
        SoundIcon::HeartPulse => "heart_pulse",
        SoundIcon::Hourglass => "hourglass",
        SoundIcon::House => "house",
        SoundIcon::Key => "key",
        SoundIcon::Landmark => "landmark",
        SoundIcon::Moon => "moon",
        SoundIcon::MoonStar => "moon_star",
        SoundIcon::Mountain => "mountain",
        SoundIcon::MountainSnow => "mountain_snow",
        SoundIcon::Music => "music",
        SoundIcon::PawPrint => "paw_print",
        SoundIcon::Piano => "piano",
        SoundIcon::Pickaxe => "pickaxe",
        SoundIcon::Rabbit => "rabbit",
        SoundIcon::Rat => "rat",
        SoundIcon::Scroll => "scroll",
        SoundIcon::Shield => "shield",
        SoundIcon::Ship => "ship",
        SoundIcon::Siren => "siren",
        SoundIcon::Skull => "skull",
        SoundIcon::Snowflake => "snowflake",
        SoundIcon::Sparkles => "sparkles",
        SoundIcon::Squirrel => "squirrel",
        SoundIcon::Sun => "sun",
        SoundIcon::Swords => "swords",
        SoundIcon::Tent => "tent",
        SoundIcon::Tornado => "tornado",
        SoundIcon::TrainFront => "train_front",
        SoundIcon::Trees => "trees",
        SoundIcon::VenetianMask => "venetian_mask",
        SoundIcon::WandSparkles => "wand_sparkles",
        SoundIcon::Waves => "waves",
        SoundIcon::Wind => "wind",
        SoundIcon::Zap => "zap",
    }
}
