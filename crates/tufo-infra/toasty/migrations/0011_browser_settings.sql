ALTER TABLE "player_settings" ADD COLUMN "browser_muted" BOOLEAN NOT NULL DEFAULT 0;
-- #[toasty::breakpoint]
ALTER TABLE "player_settings" ADD COLUMN "browser_volume" REAL NOT NULL DEFAULT 1.0 CHECK ("browser_volume" BETWEEN 0.0 AND 1.0);
