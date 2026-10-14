ALTER TABLE "collections" ADD COLUMN "pinned" BOOLEAN NOT NULL DEFAULT FALSE;
-- #[toasty::breakpoint]
-- Tufo keeps no play history, so the five newest shown collections of each kind stand in for the
-- ones in use. Timestamps are fixed-width UTC text, so their text order is their time order.
UPDATE "collections" SET "pinned" = TRUE
WHERE "id" IN (
  SELECT "id" FROM (
    SELECT "id", ROW_NUMBER() OVER (
      PARTITION BY "kind" ORDER BY "created_at" DESC, "id" DESC
    ) AS "recency"
    FROM "collections"
    WHERE "hidden_at" IS NULL
  )
  WHERE "recency" <= 5
);
