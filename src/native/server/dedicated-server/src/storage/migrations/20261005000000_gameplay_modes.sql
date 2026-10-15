-- Native compatibility identities. Legacy rooms remain unknown and are not exposed.
CREATE TABLE game_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  instance TEXT NOT NULL,
  mode INTEGER NOT NULL CHECK(mode IN(1,2)),
  source_ip TEXT NOT NULL,
  seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  retired_at TEXT,
  UNIQUE(user_id,instance)
);
CREATE UNIQUE INDEX game_runs_current ON game_runs(user_id) WHERE retired_at IS NULL;
ALTER TABLE user_sessions ADD COLUMN game_run_id INTEGER REFERENCES game_runs(id) ON DELETE CASCADE;
ALTER TABLE user_sessions ADD COLUMN ticket_hash TEXT;
ALTER TABLE user_sessions ADD COLUMN native_connected INTEGER NOT NULL DEFAULT 1 CHECK(native_connected IN(0,1));
CREATE UNIQUE INDEX user_sessions_ticket_hash ON user_sessions(ticket_hash) WHERE ticket_hash IS NOT NULL;
ALTER TABLE game_sessions ADD COLUMN gameplay_mode INTEGER NOT NULL DEFAULT 0 CHECK(gameplay_mode IN(0,1,2));
CREATE TRIGGER scbl_room_mode_on_create AFTER INSERT ON game_sessions
BEGIN
  UPDATE game_sessions SET gameplay_mode=COALESCE((SELECT mode FROM game_runs WHERE user_id=NEW.creator_id AND retired_at IS NULL),0) WHERE id=NEW.id;
END;
CREATE TRIGGER scbl_room_mode_immutable BEFORE UPDATE OF gameplay_mode ON game_sessions
WHEN OLD.gameplay_mode!=0 AND NEW.gameplay_mode!=OLD.gameplay_mode
BEGIN SELECT RAISE(ABORT,'immutable gameplay mode'); END;
-- The native gateway admits only known rooms. The mode0 branch is retained for
-- legacy rows/restoration inspection; it is never an Official fallback.
CREATE TRIGGER scbl_participant_mode_insert BEFORE INSERT ON participants
WHEN (SELECT gameplay_mode FROM game_sessions WHERE id=NEW.game_id)!=0
 AND NOT EXISTS(SELECT 1 FROM game_runs r JOIN game_sessions g ON g.id=NEW.game_id
                WHERE r.user_id=NEW.user_id AND r.retired_at IS NULL AND r.mode=g.gameplay_mode)
BEGIN SELECT RAISE(ABORT,'participant gameplay mode mismatch'); END;
CREATE TRIGGER scbl_participant_mode_update BEFORE UPDATE OF game_id,user_id ON participants
WHEN (SELECT gameplay_mode FROM game_sessions WHERE id=NEW.game_id)!=0
 AND NOT EXISTS(SELECT 1 FROM game_runs r JOIN game_sessions g ON g.id=NEW.game_id
                WHERE r.user_id=NEW.user_id AND r.retired_at IS NULL AND r.mode=g.gameplay_mode)
BEGIN SELECT RAISE(ABORT,'participant gameplay mode mismatch'); END;
CREATE TRIGGER scbl_host_mode_update BEFORE UPDATE OF creator_id ON game_sessions
WHEN NEW.gameplay_mode!=0 AND NOT EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=NEW.creator_id AND r.retired_at IS NULL AND r.mode=NEW.gameplay_mode)
BEGIN SELECT RAISE(ABORT,'host gameplay mode mismatch'); END;
CREATE TRIGGER scbl_party_mode_insert BEFORE INSERT ON party_members
WHEN EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL WHERE p.id=NEW.party_id)
 AND NOT EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL
                JOIN game_runs member ON member.user_id=NEW.user_id AND member.retired_at IS NULL AND member.mode=leader.mode WHERE p.id=NEW.party_id)
BEGIN SELECT RAISE(ABORT,'party gameplay mode mismatch'); END;
CREATE TRIGGER scbl_party_mode_update BEFORE UPDATE OF party_id,user_id ON party_members
WHEN EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL WHERE p.id=NEW.party_id)
 AND NOT EXISTS(SELECT 1 FROM parties p JOIN game_runs leader ON leader.user_id=p.leader_id AND leader.retired_at IS NULL
                JOIN game_runs member ON member.user_id=NEW.user_id AND member.retired_at IS NULL AND member.mode=leader.mode WHERE p.id=NEW.party_id)
BEGIN SELECT RAISE(ABORT,'party gameplay mode mismatch'); END;
