CREATE TABLE "player_settings" (
    "id" BIGINT NOT NULL CHECK ("id" = 1),
    "volume" REAL NOT NULL CHECK ("volume" BETWEEN 0.0 AND 1.0),
    "muted" BOOLEAN NOT NULL,
    "shuffle" BOOLEAN NOT NULL,
    "repeat" TEXT NOT NULL CHECK ("repeat" IN ('off', 'playlist', 'track')),
    "crossfade" INTEGER NOT NULL CHECK ("crossfade" BETWEEN 0 AND 10000),
    PRIMARY KEY ("id")
);
