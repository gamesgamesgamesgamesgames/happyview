-- dead_letter_scripts.payload was created as JSONB, but sqlx's AnyPool driver
-- can neither decode JSONB nor bind TEXT into it. Convert to TEXT to match
-- every other JSON column (see 20260318, 20260411).
ALTER TABLE happyview_dead_letter_scripts
    ALTER COLUMN payload TYPE TEXT USING payload::text;
