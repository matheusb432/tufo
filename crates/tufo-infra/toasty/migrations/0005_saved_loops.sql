CREATE TABLE "saved_loops" (
    "collection_entry_id" BLOB NOT NULL REFERENCES "collection_entries" ("id") ON DELETE CASCADE,
    "paused" BOOLEAN NOT NULL,
    PRIMARY KEY ("collection_entry_id")
);
