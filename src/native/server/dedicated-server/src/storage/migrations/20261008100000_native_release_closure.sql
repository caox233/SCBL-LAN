-- Additive only. Old rows remain NULL and are never relabeled by a later run.
ALTER TABLE game_sessions ADD COLUMN release_identity TEXT;
CREATE TRIGGER scbl_room_release_immutable BEFORE UPDATE OF release_identity ON game_sessions
WHEN OLD.release_identity IS NOT NEW.release_identity
BEGIN SELECT RAISE(ABORT,'immutable room release identity'); END;
CREATE TRIGGER scbl_room_release_insert BEFORE INSERT ON game_sessions
WHEN NEW.release_identity IS NOT NULL AND NOT EXISTS(
 SELECT 1 FROM game_runs r WHERE r.user_id=NEW.creator_id AND r.release_identity=NEW.release_identity
 AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'room creator release mismatch'); END;
CREATE TRIGGER scbl_participant_release_insert BEFORE INSERT ON participants
WHEN NOT EXISTS(SELECT 1 FROM game_runs r JOIN game_sessions g ON g.id=NEW.game_id
 WHERE r.user_id=NEW.user_id AND r.mode=g.gameplay_mode AND r.release_identity=g.release_identity
 AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'participant release mismatch'); END;
CREATE TRIGGER scbl_participant_release_update BEFORE UPDATE OF game_id,user_id ON participants
WHEN NOT EXISTS(SELECT 1 FROM game_runs r JOIN game_sessions g ON g.id=NEW.game_id
 WHERE r.user_id=NEW.user_id AND r.mode=g.gameplay_mode AND r.release_identity=g.release_identity
 AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'participant release mismatch'); END;
CREATE TRIGGER scbl_host_release_update BEFORE UPDATE OF creator_id ON game_sessions
WHEN NOT EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=NEW.creator_id AND r.mode=NEW.gameplay_mode
 AND r.release_identity=NEW.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'host release mismatch'); END;
CREATE TRIGGER scbl_restore_release_update BEFORE UPDATE OF destroyed_at ON game_sessions
WHEN OLD.destroyed_at IS NOT NULL AND NEW.destroyed_at IS NULL AND NOT EXISTS(
 SELECT 1 FROM game_runs r WHERE r.user_id=NEW.creator_id AND r.mode=NEW.gameplay_mode
 AND r.release_identity=NEW.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'restoration release mismatch'); END;
CREATE TRIGGER scbl_party_release_insert BEFORE INSERT ON party_members
WHEN NOT EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL
 JOIN game_runs member ON member.user_id=NEW.user_id AND member.retired_at IS NULL AND member.mode=leader.mode
 AND member.release_identity=leader.release_identity WHERE p.id=NEW.party_id
 AND leader.seen_at>=datetime('now','-90 seconds') AND member.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'party release mismatch'); END;
CREATE TRIGGER scbl_party_release_update BEFORE UPDATE OF party_id,user_id ON party_members
WHEN NOT EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL
 JOIN game_runs member ON member.user_id=NEW.user_id AND member.retired_at IS NULL AND member.mode=leader.mode
 AND member.release_identity=leader.release_identity WHERE p.id=NEW.party_id
 AND leader.seen_at>=datetime('now','-90 seconds') AND member.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'party release mismatch'); END;
-- The departed leader may be stale; the incoming leader must be current/fresh and
-- retain the old leader's non-NULL identity. Cleanup may still delete a stale party.
CREATE TRIGGER scbl_party_leader_release_update BEFORE UPDATE OF leader_id ON parties
WHEN OLD.leader_id!=NEW.leader_id AND NOT EXISTS(
 SELECT 1 FROM game_runs previous JOIN game_runs incoming ON incoming.user_id=NEW.leader_id
 WHERE previous.user_id=OLD.leader_id AND previous.retired_at IS NULL AND incoming.retired_at IS NULL
 AND incoming.mode=previous.mode AND incoming.release_identity=previous.release_identity
 AND incoming.seen_at>=datetime('now','-90 seconds'))
BEGIN SELECT RAISE(ABORT,'party leader release mismatch'); END;
