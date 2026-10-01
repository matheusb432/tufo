CREATE TABLE "kenku_imports" (
    "id" BLOB NOT NULL CHECK (length("id") = 16),
    "source_folder" TEXT,
    "state" TEXT NOT NULL CHECK ("state" IN ('reading', 'awaiting_choices', 'importing', 'succeeded', 'failed', 'interrupted')),
    "progress" INTEGER NOT NULL CHECK ("progress" BETWEEN 0 AND 100),
    "details" TEXT NOT NULL CHECK (json_valid("details")),
    "error_reason" TEXT,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    PRIMARY KEY ("id"),
    CHECK ("state" != 'succeeded' OR "progress" = 100)
);
-- #[toasty::breakpoint]
-- The database also enforces admission, including jobs paused for collection choices.
CREATE UNIQUE INDEX "kenku_imports_one_active" ON "kenku_imports" ((1))
WHERE "state" IN ('reading', 'awaiting_choices', 'importing');
-- #[toasty::breakpoint]
CREATE INDEX "kenku_imports_updated_at" ON "kenku_imports" ("updated_at" DESC);
