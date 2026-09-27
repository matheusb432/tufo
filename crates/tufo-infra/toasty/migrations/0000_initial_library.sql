CREATE TABLE "library_folders" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "kind" TEXT NOT NULL CHECK ("kind" IN ('playlist', 'soundboard')),
    "folder_path" TEXT NOT NULL CHECK (length("folder_path") > 0),
    "folder_identity" TEXT NOT NULL CHECK (length("folder_identity") BETWEEN 1 AND 100),
    "created_at" TEXT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_library_folders_by_kind_and_folder_path" ON "library_folders" ("kind", "folder_path");
-- #[toasty::breakpoint]
CREATE TABLE "audio_files" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "file_path" TEXT NOT NULL CHECK (length("file_path") > 0),
    "file_identity" TEXT NOT NULL CHECK (length("file_identity") BETWEEN 1 AND 100),
    "size_bytes" BIGINT NOT NULL CHECK ("size_bytes" >= 0),
    "modified_ns" BIGINT NOT NULL,
    "fingerprint" TEXT NOT NULL CHECK (length("fingerprint") = 64),
    "duration_ms" INTEGER NOT NULL CHECK ("duration_ms" BETWEEN 0 AND 4294967295),
    "title" TEXT NOT NULL CHECK (length("title") BETWEEN 1 AND 200),
    "missing_since" TEXT,
    "created_at" TEXT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_audio_files_by_file_path" ON "audio_files" ("file_path");
-- #[toasty::breakpoint]
CREATE INDEX "index_audio_files_by_file_identity" ON "audio_files" ("file_identity");
-- #[toasty::breakpoint]
CREATE INDEX "index_audio_files_by_fingerprint" ON "audio_files" ("fingerprint");
-- #[toasty::breakpoint]
CREATE TABLE "collections" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "kind" TEXT NOT NULL CHECK ("kind" IN ('playlist', 'soundboard')),
    "title" TEXT NOT NULL CHECK (length("title") BETWEEN 1 AND 200),
    "background_image" TEXT CHECK (length("background_image") > 0),
    "order_key" TEXT NOT NULL CHECK ("order_key" IN ('title', 'duration', 'date_added')),
    "order_descending" BOOLEAN NOT NULL,
    "library_folder_id" BLOB REFERENCES "library_folders" ("id"),
    "folder_path" TEXT CHECK (length("folder_path") > 0),
    "folder_identity" TEXT CHECK (length("folder_identity") BETWEEN 1 AND 100),
    "folder_missing_since" TEXT,
    "hidden_at" TEXT,
    "created_at" TEXT NOT NULL,
    PRIMARY KEY ("id"),
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
CREATE INDEX "index_collections_by_library_folder_id" ON "collections" ("library_folder_id");
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_collections_by_linked_folder" ON "collections" ("library_folder_id", "folder_path") WHERE "library_folder_id" IS NOT NULL;
-- #[toasty::breakpoint]
CREATE TABLE "collection_entries" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "collection_id" BLOB NOT NULL REFERENCES "collections" ("id") ON DELETE CASCADE,
    "audio_file_id" BLOB NOT NULL REFERENCES "audio_files" ("id") ON DELETE CASCADE,
    "added_at" TEXT NOT NULL,
    "looping" BOOLEAN NOT NULL,
    "volume" REAL NOT NULL CHECK ("volume" BETWEEN 0.0 AND 1.0),
    "fade_in" INTEGER NOT NULL CHECK ("fade_in" BETWEEN 0 AND 60000),
    "fade_out" INTEGER NOT NULL CHECK ("fade_out" BETWEEN 0 AND 60000),
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_collection_entries_by_collection_id" ON "collection_entries" ("collection_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_collection_entries_by_audio_file_id" ON "collection_entries" ("audio_file_id");
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_collection_entries_by_collection_and_audio_file" ON "collection_entries" ("collection_id", "audio_file_id");
