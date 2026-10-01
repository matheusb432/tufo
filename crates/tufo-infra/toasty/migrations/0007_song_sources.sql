ALTER TABLE "audio_files" ADD COLUMN "song_source" BOOLEAN NOT NULL DEFAULT FALSE;
-- #[toasty::breakpoint]
-- Every file added to a hand-picked playlist was added by its own path.
UPDATE "audio_files" SET "song_source" = TRUE
WHERE EXISTS (
  SELECT 1 FROM "collection_entries" AS entry
  JOIN "collections" AS collection ON collection."id" = entry."collection_id"
  WHERE entry."audio_file_id" = "audio_files"."id"
    AND collection."kind" = 'playlist'
    AND collection."library_folder_id" IS NULL
);
