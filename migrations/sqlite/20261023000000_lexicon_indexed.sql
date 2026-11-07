-- Whether HappyView ingests this record collection from the network. A
-- lexicon kept only to validate writes and serve refs is installed with 0.
ALTER TABLE happyview_lexicons ADD COLUMN indexed INTEGER NOT NULL DEFAULT 1;
