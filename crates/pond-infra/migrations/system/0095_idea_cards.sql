-- An idea as a short card: what kind of thing it is, the card's one line, the facts its page
-- shows at a glance, and the notes Goose keeps to work from when the member asks about it.
ALTER TABLE ideas ADD COLUMN kind TEXT NOT NULL DEFAULT 'interest';
ALTER TABLE ideas ADD COLUMN hook TEXT NOT NULL DEFAULT '';
ALTER TABLE ideas ADD COLUMN facts_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE ideas ADD COLUMN notes TEXT NOT NULL DEFAULT '';
