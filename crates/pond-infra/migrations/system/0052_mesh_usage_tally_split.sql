-- Split mesh_usage_tally's single undirected counter into borrowed/lent.
--
-- mesh_usage_tally originally had one `pending_tokens` column that both
-- lending and borrowing wrote to — a settlement pass could end up paying a
-- peer for work THEY owed US. This migration replaces it with
-- tokens_borrowed (what we owe the peer; settlement pays this) and
-- tokens_lent (what the peer owes us; their job to settle, not ours).
--
-- This is a separate migration rather than an edit to 0046_mesh_ledger.sql
-- on purpose: 0046 had already shipped and been applied on real installs,
-- and sqlx hard-fails startup on any content change to an already-applied
-- migration (checksum mismatch) rather than skipping ahead to later ones.
ALTER TABLE mesh_usage_tally ADD COLUMN tokens_borrowed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mesh_usage_tally ADD COLUMN tokens_lent INTEGER NOT NULL DEFAULT 0;
UPDATE mesh_usage_tally SET tokens_borrowed = pending_tokens;
ALTER TABLE mesh_usage_tally DROP COLUMN pending_tokens;
