ALTER TABLE "player_settings" ADD COLUMN "effects_volume" REAL NOT NULL DEFAULT 1.0 CHECK ("effects_volume" BETWEEN 0.0 AND 1.0);
