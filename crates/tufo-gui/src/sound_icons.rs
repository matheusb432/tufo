//! Each [`SoundIcon`]'s Lucide drawing and picker label.

use slint::Image;
use tufo_models::library::SoundIcon;

macro_rules! sound_icon_drawings {
    ($($variant:ident => $file:literal, $label:literal;)+) => {
        fn svg(icon: SoundIcon) -> &'static [u8] {
            match icon {
                $(SoundIcon::$variant => include_bytes!(concat!("../icons/sounds/", $file, ".svg")),)+
            }
        }

        pub(crate) fn label(icon: SoundIcon) -> &'static str {
            match icon {
                $(SoundIcon::$variant => crate::i18n::tr($label),)+
            }
        }
    };
}

sound_icon_drawings! {
    AudioLines => "audio-lines", "Sound";
    Anchor => "anchor", "Harbor";
    Axe => "axe", "Axe";
    Beer => "beer", "Tavern";
    Bell => "bell", "Bell";
    Bird => "bird", "Birds";
    Bomb => "bomb", "Explosion";
    Bone => "bone", "Bones";
    BowArrow => "bow-arrow", "Arrows";
    Bug => "bug", "Insects";
    Building => "building-2", "City";
    Castle => "castle", "Castle";
    Cat => "cat", "Cat";
    Church => "church", "Church";
    Clock => "clock", "Clock";
    CloudFog => "cloud-fog", "Fog";
    CloudLightning => "cloud-lightning", "Storm";
    CloudRain => "cloud-rain", "Rain";
    CloudSnow => "cloud-snow", "Snow";
    Coffee => "coffee", "Coffee shop";
    Coins => "coins", "Coins";
    Crown => "crown", "Court";
    Dices => "dices", "Dice";
    Dog => "dog", "Dog";
    DoorOpen => "door-open", "Door";
    Droplets => "droplets", "Stream";
    Drum => "drum", "Drums";
    Fan => "fan", "Fan";
    Fish => "fish", "Fish";
    Flame => "flame", "Fire";
    FlameKindling => "flame-kindling", "Fireplace";
    FlaskConical => "flask-conical", "Alchemy";
    Footprints => "footprints", "Footsteps";
    Ghost => "ghost", "Ghost";
    Guitar => "guitar", "Guitar";
    Hammer => "hammer", "Forge";
    HeartPulse => "heart-pulse", "Heartbeat";
    Hourglass => "hourglass", "Hourglass";
    House => "house", "House";
    Key => "key", "Key";
    Landmark => "landmark", "Temple";
    Moon => "moon", "Night";
    MoonStar => "moon-star", "Summer night";
    Mountain => "mountain", "Mountain";
    MountainSnow => "mountain-snow", "Peak";
    Music => "music", "Music";
    PawPrint => "paw-print", "Beast";
    Piano => "piano", "Piano";
    Pickaxe => "pickaxe", "Mine";
    Rabbit => "rabbit", "Rabbit";
    Rat => "rat", "Rats";
    Scroll => "scroll", "Scroll";
    Shield => "shield", "Shield";
    Ship => "ship", "Ship";
    Siren => "siren", "Alarm";
    Skull => "skull", "Skull";
    Snowflake => "snowflake", "Frost";
    Sparkles => "sparkles", "Magic";
    Squirrel => "squirrel", "Squirrel";
    Sun => "sun", "Day";
    Swords => "swords", "Battle";
    Tent => "tent", "Camp";
    Tornado => "tornado", "Tornado";
    TrainFront => "train-front", "Train";
    Trees => "trees", "Forest";
    VenetianMask => "venetian-mask", "Masquerade";
    WandSparkles => "wand-sparkles", "Spell";
    Waves => "waves", "Waves";
    Wind => "wind", "Wind";
    Zap => "zap", "Lightning";
}

/// In [`SoundIcon::ALL`]'s order.
pub(crate) fn images() -> Vec<Image> {
    SoundIcon::ALL
        .iter()
        .map(|&icon| {
            Image::load_from_svg_data(svg(icon)).unwrap_or_else(|error| {
                tracing::warn!(?icon, %error, "a sound icon's drawing does not parse");
                Image::default()
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws() {
        for &icon in SoundIcon::ALL {
            let image = Image::load_from_svg_data(svg(icon)).unwrap();

            assert_eq!(image.size().width, 24, "{icon:?}");
        }
    }
}
