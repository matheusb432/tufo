ALTER TABLE "collections" ADD COLUMN "background_preset" TEXT
    CHECK ("background_preset" IN ('autumn', 'battle', 'castle', 'cathedral', 'coast', 'desert', 'dragon', 'fire', 'forest', 'haunted', 'mountain', 'night', 'sky', 'skulls', 'storm', 'tavern', 'town', 'underworld', 'volcano', 'winter'))
    CONSTRAINT "collections_show_one_background" CHECK ("background_preset" IS NULL OR "background_image" IS NULL);
