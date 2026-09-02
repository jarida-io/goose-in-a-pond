-- Persist the last-known dial address for a trusted mesh peer, so the
-- background auto-reconnect loop (pond-adapters-mesh-libp2p's
-- `retry_disconnected_known_peers`) has something to redial with after a
-- process restart. Previously this lived only in the swarm task's in-memory
-- map, so a restart lost it and reconnecting needed a fresh invite/address.
ALTER TABLE mesh_trusted_peers ADD COLUMN last_known_address TEXT;
