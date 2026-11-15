-- A device a test harness paired. It gets no household notices, its pairing is not announced,
-- and its traffic is left out of the reliability report. A code minted as a test code marks the
-- device that redeems it.
ALTER TABLE devices ADD COLUMN is_test INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pairing_codes ADD COLUMN is_test INTEGER NOT NULL DEFAULT 0;
