ALTER TABLE "collections" ADD COLUMN "builtin_title" TEXT CHECK ("builtin_title" IN ('effects', 'imported', 'rain', 'storm', 'wind', 'waves', 'stream', 'birds', 'summer_night', 'fireplace'));
-- #[toasty::breakpoint]
ALTER TABLE "audio_files" ADD COLUMN "builtin_title" TEXT CHECK ("builtin_title" IN ('effects', 'imported', 'rain', 'storm', 'wind', 'waves', 'stream', 'birds', 'summer_night', 'fireplace'));
-- #[toasty::breakpoint]
-- The seed timestamp identifies entries created by Tufo, without treating user titles as keys.
UPDATE "collections" SET "builtin_title" = 'effects'
WHERE "title" = 'Effects' AND "library_folder_id" IS NULL
  AND EXISTS (
    SELECT 1 FROM "collection_entries" AS entry
    JOIN "default_effects_seed" AS seed ON entry."added_at" = seed."seeded_at"
    WHERE entry."collection_id" = "collections"."id"
  );
-- #[toasty::breakpoint]
UPDATE "audio_files" SET "builtin_title" = CASE "title"
  WHEN 'Rain' THEN 'rain'
  WHEN 'Storm' THEN 'storm'
  WHEN 'Wind' THEN 'wind'
  WHEN 'Waves' THEN 'waves'
  WHEN 'Stream' THEN 'stream'
  WHEN 'Birds' THEN 'birds'
  WHEN 'Summer Night' THEN 'summer_night'
  WHEN 'Fireplace' THEN 'fireplace'
END
WHERE EXISTS (
  SELECT 1 FROM "collection_entries" AS entry
  JOIN "default_effects_seed" AS seed ON entry."added_at" = seed."seeded_at"
  WHERE entry."audio_file_id" = "audio_files"."id"
);
