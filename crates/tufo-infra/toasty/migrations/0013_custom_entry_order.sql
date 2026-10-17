ALTER TABLE "collection_entries" ADD COLUMN "position" INTEGER NOT NULL DEFAULT 0 CHECK ("position" BETWEEN 0 AND 4294967295);
-- #[toasty::breakpoint]
-- The custom order starts as each collection's current order. SQL can't compare titles the way
-- the library sorts them, so a title-ordered collection's entries share a position and fall back
-- to the title. Timestamps are fixed-width UTC text, so their text order is their time order.
UPDATE "collection_entries" SET "position" = ranked."position"
FROM (
  SELECT entry."id", RANK() OVER (
    PARTITION BY entry."collection_id"
    ORDER BY
      CASE WHEN collection."order_key" = 'duration' AND NOT collection."order_descending"
        THEN file."duration_ms" END ASC,
      CASE WHEN collection."order_key" = 'duration' AND collection."order_descending"
        THEN file."duration_ms" END DESC,
      CASE WHEN collection."order_key" = 'date_added' AND NOT collection."order_descending"
        THEN file."created_at" END ASC,
      CASE WHEN collection."order_key" = 'date_added' AND collection."order_descending"
        THEN file."created_at" END DESC
  ) - 1 AS "position"
  FROM "collection_entries" AS entry
  JOIN "collections" AS collection ON collection."id" = entry."collection_id"
  JOIN "audio_files" AS file ON file."id" = entry."audio_file_id"
) AS ranked
WHERE ranked."id" = "collection_entries"."id";
-- #[toasty::breakpoint]
-- Widening the order_key check rebuilds the table. The migration connection leaves foreign keys
-- off, so dropping the old table keeps the entries that reference it.
CREATE TABLE "_new_collections" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "kind" TEXT NOT NULL CHECK ("kind" IN ('playlist', 'soundboard')),
    "title" TEXT NOT NULL CHECK (length("title") BETWEEN 1 AND 200),
    "background_image" TEXT CHECK (length("background_image") > 0),
    "order_key" TEXT NOT NULL CHECK ("order_key" IN ('title', 'duration', 'date_added', 'custom')),
    "order_descending" BOOLEAN NOT NULL,
    "library_folder_id" BLOB REFERENCES "library_folders" ("id"),
    "folder_path" TEXT CHECK (length("folder_path") > 0),
    "folder_identity" TEXT CHECK (length("folder_identity") BETWEEN 1 AND 100),
    "folder_missing_since" TEXT,
    "hidden_at" TEXT,
    "created_at" TEXT NOT NULL,
    "background_preset" TEXT CHECK ("background_preset" IN ('autumn', 'battle', 'castle', 'cathedral', 'coast', 'desert', 'dragon', 'fire', 'forest', 'haunted', 'mountain', 'night', 'sky', 'skulls', 'storm', 'tavern', 'town', 'underworld', 'volcano', 'winter')),
    "builtin_title" TEXT CHECK ("builtin_title" IN ('effects', 'imported', 'rain', 'storm', 'wind', 'waves', 'stream', 'birds', 'summer_night', 'fireplace')),
    "pinned" BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY ("id"),
    CONSTRAINT "collections_show_one_background" CHECK ("background_preset" IS NULL OR "background_image" IS NULL),
    CONSTRAINT "linked_collections_have_a_folder" CHECK (
        ("library_folder_id" IS NULL) = ("folder_path" IS NULL)
        AND ("library_folder_id" IS NULL) = ("folder_identity" IS NULL)
    ),
    CONSTRAINT "only_linked_collections_miss_or_hide" CHECK (
        "library_folder_id" IS NOT NULL
        OR ("folder_missing_since" IS NULL AND "hidden_at" IS NULL)
    )
);
-- #[toasty::breakpoint]
INSERT INTO "_new_collections" ("id", "kind", "title", "background_image", "order_key", "order_descending", "library_folder_id", "folder_path", "folder_identity", "folder_missing_since", "hidden_at", "created_at", "background_preset", "builtin_title", "pinned")
SELECT "id", "kind", "title", "background_image", "order_key", "order_descending", "library_folder_id", "folder_path", "folder_identity", "folder_missing_since", "hidden_at", "created_at", "background_preset", "builtin_title", "pinned" FROM "collections";
-- #[toasty::breakpoint]
DROP TABLE "collections";
-- #[toasty::breakpoint]
ALTER TABLE "_new_collections" RENAME TO "collections";
-- #[toasty::breakpoint]
CREATE INDEX "index_collections_by_library_folder_id" ON "collections" ("library_folder_id");
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_collections_by_linked_folder" ON "collections" ("library_folder_id", "folder_path") WHERE "library_folder_id" IS NOT NULL;
