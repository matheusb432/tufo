use std::{collections::BTreeSet, str::FromStr};

use serde::Deserialize;
use tufo_client::library::{ImportKenkuLibrary, KenkuCollectionChoice};
use tufo_models::{kenku_import::KenkuImportId, library::CollectionKind, path::FolderPath};

#[derive(Debug, Clone)]
pub(crate) struct KenkuStart(ImportKenkuLibrary);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KenkuStartJson {
    kenku_folder: Option<String>,
}

impl FromStr for KenkuStart {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let raw: KenkuStartJson = crate::input::parse_json(text)?;
        Ok(Self(ImportKenkuLibrary {
            kenku_folder: folder(raw.kenku_folder)?,
            ..ImportKenkuLibrary::default()
        }))
    }
}

impl KenkuStart {
    pub(super) fn into_request(self) -> ImportKenkuLibrary {
        self.0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct KenkuResume(ImportKenkuLibrary);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KenkuResumeJson {
    import_id: String,
    kenku_folder: Option<String>,
    #[serde(default)]
    choices: Vec<KenkuCollectionChoiceJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KenkuCollectionChoiceJson {
    source_id: String,
    kind: CollectionKindJson,
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CollectionKindJson {
    Playlist,
    Soundboard,
}

impl From<CollectionKindJson> for CollectionKind {
    fn from(kind: CollectionKindJson) -> Self {
        match kind {
            CollectionKindJson::Playlist => Self::Playlist,
            CollectionKindJson::Soundboard => Self::Soundboard,
        }
    }
}

impl FromStr for KenkuResume {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let raw: KenkuResumeJson = crate::input::parse_json(text)?;
        let import_id = raw
            .import_id
            .parse()
            .map(KenkuImportId::from_uuid)
            .map_err(|_| "import_id must be a UUID".to_owned())?;
        let kenku_folder = folder(raw.kenku_folder)?;
        let mut seen = BTreeSet::new();
        let mut choices = Vec::with_capacity(raw.choices.len());
        for choice in raw.choices {
            if choice.source_id.trim().is_empty() {
                return Err("choices[].source_id must not be empty".to_owned());
            }
            let kind = CollectionKind::from(choice.kind);
            if !seen.insert((kind, choice.source_id.clone())) {
                return Err("choices must not repeat the same kind and source_id".to_owned());
            }
            choices.push(KenkuCollectionChoice {
                source_id: choice.source_id,
                kind,
                overwrite: choice.overwrite,
            });
        }
        Ok(Self(ImportKenkuLibrary {
            kenku_folder,
            import_id: Some(import_id),
            choices,
        }))
    }
}

impl KenkuResume {
    pub(super) fn into_request(self) -> ImportKenkuLibrary {
        self.0
    }
}

fn folder(path: Option<String>) -> Result<Option<FolderPath>, String> {
    path.map(|path| FolderPath::try_new(path.into()))
        .transpose()
        .map_err(|_| "kenku_folder must be an absolute UTF-8 path".to_owned())
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;
    use clap::{Parser as _, error::ErrorKind};
    use serde_json::{Value, json};

    use super::*;

    const IMPORT_ID: &str = "00000000-0000-0000-0000-000000000001";

    #[test]
    fn default_and_explicit_folders_become_domain_requests() -> anyhow::Result<()> {
        for text in ["{}", r#"{"kenku_folder":null}"#] {
            let request = text
                .parse::<KenkuStart>()
                .map_err(anyhow::Error::msg)?
                .into_request();
            assert_eq!(request, ImportKenkuLibrary::default());
        }
        let path = if cfg!(windows) {
            r"C:\Kenku\data"
        } else {
            "/srv/kenku-data"
        };
        let payload = json!({"kenku_folder": path});
        let request = payload
            .to_string()
            .parse::<KenkuStart>()
            .map_err(anyhow::Error::msg)?
            .into_request();
        assert_eq!(
            request
                .kenku_folder
                .context("missing explicit folder")?
                .as_ref(),
            std::path::Path::new(path)
        );
        assert_eq!(request.import_id, None);
        assert_eq!(request.choices, []);
        Ok(())
    }

    #[test]
    fn resume_choices_are_explicit_and_omission_defers_to_saved_choices() -> anyhow::Result<()> {
        let payload = json!({
            "import_id": IMPORT_ID,
            "choices": [
                {"source_id": "shared-source", "kind": "playlist", "overwrite": false},
                {"source_id": "shared-source", "kind": "soundboard", "overwrite": true},
            ],
        });
        let request = payload
            .to_string()
            .parse::<KenkuResume>()
            .map_err(anyhow::Error::msg)?
            .into_request();
        assert_eq!(
            request.import_id.context("missing import ID")?.to_string(),
            IMPORT_ID
        );
        assert_eq!(request.choices.len(), 2);
        assert_eq!(request.choices[0].kind, CollectionKind::Playlist);
        assert!(!request.choices[0].overwrite);
        assert_eq!(request.choices[1].kind, CollectionKind::Soundboard);
        assert!(request.choices[1].overwrite);

        let payload = json!({"import_id": IMPORT_ID});
        let request = payload
            .to_string()
            .parse::<KenkuResume>()
            .map_err(anyhow::Error::msg)?
            .into_request();
        assert_eq!(request.choices, []);
        assert_eq!(request.kenku_folder, None);
        Ok(())
    }

    #[test]
    fn invalid_starts_and_nested_choices_are_clap_value_errors() -> anyhow::Result<()> {
        let choices = json!([
            {"source_id": "source-1", "kind": "playlist", "overwrite": false},
        ]);
        let base = json!({"import_id": IMPORT_ID, "choices": choices});
        let mut cases = vec![
            ("start", json!({"kenku_folder": "relative"})),
            ("start", json!({"import_id": IMPORT_ID})),
            ("start", json!({"typo": true})),
            ("resume", json!({"import_id": "not-a-uuid"})),
            ("resume", json!({"import_id": IMPORT_ID, "choices": null})),
            (
                "resume",
                json!({"import_id": IMPORT_ID, "choices": [
                    {"source_id": "source-1", "kind": "playlist", "overwrite": false},
                    {"source_id": "source-1", "kind": "playlist", "overwrite": true},
                ]}),
            ),
        ];
        for (pointer, value) in [
            ("/choices/0/source_id", json!(" ")),
            ("/choices/0/kind", json!("other")),
            ("/choices/0/overwrite", Value::Null),
        ] {
            let mut payload = base.clone();
            *payload
                .pointer_mut(pointer)
                .context("missing conflict choice field")? = value;
            cases.push(("resume", payload));
        }
        let mut extra = base.clone();
        extra["choices"][0]["typo"] = json!(true);
        cases.push(("resume", extra));
        let mut missing_overwrite = base;
        missing_overwrite["choices"][0]
            .as_object_mut()
            .context("conflict choice is not an object")?
            .remove("overwrite");
        cases.push(("resume", missing_overwrite));
        for (command, payload) in cases {
            let text = payload.to_string();
            let error = crate::command::Cli::try_parse_from(["tufo", "kenku", command, &text])
                .err()
                .with_context(|| format!("invalid Kenku input was accepted: {payload}"))?;
            assert_eq!(
                error.kind(),
                ErrorKind::ValueValidation,
                "{payload}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn retry_uses_resume_and_deadlines_reject_zero_before_connect() -> anyhow::Result<()> {
        let payload = json!({"import_id": IMPORT_ID}).to_string();
        let parsed = crate::command::Cli::try_parse_from(["tufo", "kenku", "retry", &payload])?;
        assert!(matches!(
            parsed.command,
            crate::command::Command::Kenku {
                command: crate::kenku_command::KenkuCommand::Resume { .. },
            }
        ));
        let error = crate::command::Cli::try_parse_from([
            "tufo",
            "kenku",
            "resume",
            &payload,
            "--timeout-ms",
            "0",
        ])
        .err()
        .context("a zero completion deadline was accepted")?;
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        Ok(())
    }
}
