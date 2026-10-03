CREATE TABLE "song_loops" (
    "audio_file_id" BLOB NOT NULL REFERENCES "audio_files" ("id") ON DELETE CASCADE,
    "fingerprint" TEXT NOT NULL CHECK (length("fingerprint") = 64),
    "sample_rate" INTEGER NOT NULL CHECK ("sample_rate" > 0),
    "frame_count" INTEGER NOT NULL,
    "playback_start_frame" INTEGER NOT NULL CHECK ("playback_start_frame" >= 0),
    "loop_start_frame" INTEGER NOT NULL,
    "loop_end_frame" INTEGER NOT NULL,
    "crossfade_frames" INTEGER NOT NULL CHECK ("crossfade_frames" >= 0),
    "updated_at" TEXT NOT NULL,
    PRIMARY KEY ("audio_file_id"),
    CONSTRAINT "markers_are_ordered_inside_the_file" CHECK (
        "playback_start_frame" <= "loop_start_frame"
        AND "loop_start_frame" < "loop_end_frame"
        AND "loop_end_frame" <= "frame_count"
    ),
    -- At most ten seconds, the loop's length, and the audio before the loop start.
    CONSTRAINT "crossfade_fits_the_loop" CHECK (
        "crossfade_frames" <= "sample_rate" * 10
        AND "crossfade_frames" <= "loop_end_frame" - "loop_start_frame"
        AND "crossfade_frames" <= "loop_start_frame"
    )
);
