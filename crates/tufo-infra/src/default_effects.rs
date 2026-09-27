//! The default effects, embedded from `assets/effects/` and written to disk for the library to
//! point at. `assets/effects/CREDITS.md` credits each.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use tufo_application::library::seed_default_effects::DefaultEffectFile;
use tufo_models::{library::default_effects::DefaultEffect, playback::AudioFilePath};

const EFFECTS_FOLDER_NAME: &str = "effects";
const EFFECT_EXTENSION: &str = "ogg";

fn ogg_bytes(effect: DefaultEffect) -> &'static [u8] {
    macro_rules! effect {
        ($name:literal) => {
            include_bytes!(concat!("../../../assets/effects/", $name, ".ogg")).as_slice()
        };
    }
    match effect {
        DefaultEffect::Rain => effect!("rain"),
        DefaultEffect::Storm => effect!("storm"),
        DefaultEffect::Wind => effect!("wind"),
        DefaultEffect::Waves => effect!("waves"),
        DefaultEffect::Stream => effect!("stream"),
        DefaultEffect::Birds => effect!("birds"),
        DefaultEffect::SummerNight => effect!("summer-night"),
        DefaultEffect::Fireplace => effect!("fireplace"),
    }
}

#[must_use]
pub fn effects_folder_beside(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or(database_path)
        .join(EFFECTS_FOLDER_NAME)
}

/// Writes each effect to `folder` as `<title>.ogg`. Blocks the calling thread.
pub fn write_default_effects(folder: &Path) -> anyhow::Result<Vec<DefaultEffectFile>> {
    fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let folder =
        fs::canonicalize(folder).with_context(|| format!("resolving {}", folder.display()))?;
    DefaultEffect::ALL
        .into_iter()
        .map(|effect| {
            let path = folder.join(format!("{}.{EFFECT_EXTENSION}", effect.title()));
            write_unless_same_size(&path, ogg_bytes(effect))?;
            let path = AudioFilePath::try_new(path.clone()).map_err(|error| {
                anyhow::anyhow!("{} is not an audio file path: {error}", path.display())
            })?;
            Ok(DefaultEffectFile { effect, path })
        })
        .collect()
}

fn write_unless_same_size(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let same_size = match fs::metadata(path) {
        Ok(metadata) => u64::try_from(bytes.len()).is_ok_and(|size| metadata.len() == size),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if !same_size {
        fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clone without Git LFS embeds pointer files, which aren't Ogg streams.
    #[test]
    fn every_effect_is_an_embedded_ogg_stream() {
        for effect in DefaultEffect::ALL {
            assert!(ogg_bytes(effect).starts_with(b"OggS"), "{effect:?}");
        }
    }

    #[test]
    fn writing_again_keeps_the_files_and_restores_a_damaged_one() {
        let folder = tempfile::tempdir().unwrap();
        let written = write_default_effects(folder.path()).unwrap();
        let rain = written
            .iter()
            .find(|file| file.effect == DefaultEffect::Rain)
            .unwrap();
        fs::write(rain.path.as_ref(), b"truncated").unwrap();

        let rewritten = write_default_effects(folder.path()).unwrap();

        assert_eq!(rewritten, written);
        assert_eq!(
            fs::read(rain.path.as_ref()).unwrap(),
            ogg_bytes(DefaultEffect::Rain)
        );
        assert_eq!(rain.path.as_ref().file_name().unwrap(), "Rain.ogg");
    }
}
