use std::{os::unix::fs::symlink, path::Path};

use anyhow::Context as _;
use serde_json::{Value, json};

use super::{
    fixtures::write_wav,
    support::{Session, identifier},
};

#[tokio::test]
async fn imports_through_parent_components_and_symlinks_return_canonical_resource_ids()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    let file = session.path("audio/files/thunder.wav");
    let file_bytes = write_wav(&file)?;
    let folder = session.path("audio/encounter");
    let folder_file = folder.join("dragon.wav");
    let folder_file_bytes = write_wav(&folder_file)?;
    let canonical_file = std::fs::canonicalize(&file)?;
    let canonical_folder = std::fs::canonicalize(&folder)?;
    let canonical_folder_file = std::fs::canonicalize(&folder_file)?;
    std::fs::create_dir_all(session.path("aliases"))?;
    let file_alias = session.path("aliases/thunder.wav");
    let folder_alias = session.path("aliases/encounter");
    symlink(&file, &file_alias)?;
    symlink(&folder, &folder_alias)?;
    session.start().await?;

    let mut first_file_resources: Option<Value> = None;
    for (path, expected_file, expected_folder) in [
        (
            session.path("audio/files/../files/thunder.wav"),
            canonical_file.as_path(),
            None,
        ),
        (file_alias.clone(), canonical_file.as_path(), None),
        (
            folder_alias.clone(),
            canonical_folder_file.as_path(),
            Some(canonical_folder.as_path()),
        ),
    ] {
        let imported = session
            .mutation(
                &["library", "import"],
                json!({"kind": "playlist", "paths": [path]}),
            )
            .await?;
        assert_canonical_resources(&session, &imported, expected_file, expected_folder).await?;
        if expected_folder.is_some() {
            continue;
        }
        if let Some(first) = &first_file_resources {
            assert_alias_matches_include_existing(first, &imported)?;
        } else {
            first_file_resources = Some(imported.clone());
        }
    }

    let manual = session
        .mutation(
            &["collection", "add"],
            json!({"kind": "playlist", "title": "CLI alias target"}),
        )
        .await?;
    let target_id = identifier(&manual)?;
    let added = session
        .mutation(
            &["collection", "add-files", target_id],
            json!({"paths": [file_alias]}),
        )
        .await?;
    assert_canonical_resources(&session, &added, &canonical_file, None).await?;
    assert_eq!(added["resources"]["collection_ids"], json!([target_id]));
    assert_eq!(
        added["resources"]["audio_file_ids"],
        first_file_resources.context("missing first import")?["resources"]["audio_file_ids"]
    );
    assert_eq!(std::fs::read(&file)?, file_bytes);
    assert_eq!(std::fs::read(&file_alias)?, file_bytes);
    assert_eq!(std::fs::read(&folder_file)?, folder_file_bytes);
    assert_eq!(
        std::fs::read(folder_alias.join("dragon.wav"))?,
        folder_file_bytes
    );
    assert!(file_alias.symlink_metadata()?.file_type().is_symlink());
    assert!(folder_alias.symlink_metadata()?.file_type().is_symlink());
    assert!(session.player().await?.music.is_none());
    assert_eq!(
        session.json(&["effect", "list"]).await?["sounds"],
        json!([])
    );
    Ok(())
}

fn resource_ids<'a>(record: &'a Value, key: &str) -> anyhow::Result<&'a [Value]> {
    let ids = record["resources"][key]
        .as_array()
        .with_context(|| format!("missing resources.{key}"))?;
    anyhow::ensure!(!ids.is_empty(), "no matching resources.{key}");
    Ok(ids)
}

fn assert_alias_matches_include_existing(first: &Value, alias: &Value) -> anyhow::Result<()> {
    assert_eq!(alias["report"]["collections_created"], 1);
    assert_eq!(
        alias["resources"]["audio_file_ids"],
        first["resources"]["audio_file_ids"]
    );
    for key in ["collection_ids", "entry_ids", "audio_file_ids"] {
        let previous = resource_ids(first, key)?;
        let current = resource_ids(alias, key)?;
        assert!(
            previous.iter().all(|id| current.contains(id)),
            "existing resources.{key} disappeared"
        );
    }
    for key in ["collection_ids", "entry_ids"] {
        assert!(
            resource_ids(alias, key)?.len() > resource_ids(first, key)?.len(),
            "alias import returned no new resources.{key}"
        );
    }
    Ok(())
}

async fn assert_canonical_resources(
    session: &Session,
    imported: &Value,
    expected_file: &Path,
    expected_folder: Option<&Path>,
) -> anyhow::Result<()> {
    let collection_ids = resource_ids(imported, "collection_ids")?;
    let entry_ids = resource_ids(imported, "entry_ids")?;
    let audio_file_ids = resource_ids(imported, "audio_file_ids")?;
    let snapshot = session.json(&["library", "snapshot"]).await?;
    let collections = snapshot["collections"]
        .as_array()
        .context("missing collections")?;
    for id in collection_ids {
        let collection = collections
            .iter()
            .find(|collection| collection["id"] == *id)
            .context("returned collection ID is absent from the library")?;
        let entries = collection["entries"]
            .as_array()
            .context("missing entries")?;
        assert!(
            entries
                .iter()
                .any(|entry| entry_ids.contains(&entry["id"])
                    && entry["path"] == json!(expected_file))
        );
        if let Some(folder) = expected_folder {
            assert_eq!(collection["source"]["folder"], json!(folder));
        }
    }
    for id in entry_ids {
        let entry = collections
            .iter()
            .filter(|collection| collection_ids.contains(&collection["id"]))
            .flat_map(|collection| collection["entries"].as_array().into_iter().flatten())
            .find(|entry| entry["id"] == *id)
            .context("returned entry ID is absent from its matching collections")?;
        assert!(audio_file_ids.contains(&entry["audio_file_id"]));
        assert_eq!(entry["path"], json!(expected_file));
    }
    let songs = snapshot["songs"].as_array().context("missing songs")?;
    for id in audio_file_ids {
        let song = songs
            .iter()
            .find(|song| song["audio_file_id"] == *id)
            .context("returned audio file ID is absent from the library")?;
        assert_eq!(song["path"], json!(expected_file));
    }
    if let Some(folder) = expected_folder {
        let folders = snapshot["library_folders"]
            .as_array()
            .context("missing library folders")?;
        for id in resource_ids(imported, "folder_ids")? {
            let found = folders
                .iter()
                .find(|folder| folder["id"] == *id)
                .context("returned folder ID is absent from the library")?;
            assert_eq!(found["folder"], json!(folder));
        }
    }
    Ok(())
}
