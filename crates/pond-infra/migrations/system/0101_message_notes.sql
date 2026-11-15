-- What is kept under a message besides its words: where a message the pond wrote by itself came
-- from, what the turn's tools really set, a suggestion's reason and offer. JSON; NULL for none.
ALTER TABLE session_messages ADD COLUMN note_json TEXT;
