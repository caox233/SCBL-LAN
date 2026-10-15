-- Additive only: legacy bindings stay unknown. Never backfill with current release.
ALTER TABLE game_runs ADD COLUMN release_identity TEXT;
-- Immutable even for NULL legacy rows. A new nonce is required, not an UPDATE.
CREATE TRIGGER scbl_run_release_immutable BEFORE UPDATE OF release_identity ON game_runs
WHEN OLD.release_identity IS NOT NEW.release_identity
BEGIN SELECT RAISE(ABORT,'immutable game run release identity'); END;
