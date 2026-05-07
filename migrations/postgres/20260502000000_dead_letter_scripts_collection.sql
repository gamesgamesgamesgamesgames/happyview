ALTER TABLE dead_letter_scripts ADD COLUMN collection TEXT;

CREATE INDEX idx_dead_letter_scripts_collection
    ON dead_letter_scripts (collection, resolved_at);
