-- New SCBL LAN schema, structure only. No old player records.
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password TEXT,
    password_hash TEXT,
    ubi_id TEXT UNIQUE,
    last_login TEXT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
, is_online INTEGER CHECK (is_online IN (0,1)) DEFAULT 0, last_api_seen_at DATETIME, recovery_code_hash TEXT NULL, auth_version INTEGER NOT NULL DEFAULT 1 CHECK (auth_version >= 1));
CREATE TABLE game_sessions (
  id INTEGER PRIMARY KEY,
  type_id INTEGER NOT NULL,
  attributes TEXT,
  creator_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  destroyed_at TEXT,
  is_private INTEGER CHECK (is_private IN (0,1))
, host_leave_pending INTEGER NOT NULL DEFAULT 0 CHECK (host_leave_pending IN (0, 1)));
CREATE TABLE participants (
  game_id INTEGER REFERENCES game_sessions(id) ON DELETE CASCADE,
  user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  UNIQUE (game_id, user_id)
);
CREATE TABLE station_urls (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  
  url TEXT NOT NULL,
  UNIQUE (user_id, url)
);
CREATE TABLE user_sessions (
  id TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE parties (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    leader_id INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE party_members (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    party_id INTEGER NOT NULL REFERENCES parties(id) ON DELETE CASCADE,
    joined_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE "invites" (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id BETWEEN 1 AND 4294967295),
    sender INTEGER NOT NULL REFERENCES users(id),
    receiver INTEGER NOT NULL REFERENCES users(id),
    created DATETIME DEFAULT CURRENT_TIMESTAMP,
    session_type INTEGER,
    session_id INTEGER,
    delivered_at DATETIME,
    consumed_at DATETIME,
    expires_at DATETIME,
    kind INTEGER NOT NULL DEFAULT 1,
    accepted_at DATETIME,
    delivery_count INTEGER NOT NULL DEFAULT 0,
    party_id INTEGER
, follow_session_type INTEGER, follow_session_id INTEGER);
CREATE INDEX users_api_presence
    ON users(last_api_seen_at);
CREATE INDEX party_members_party ON party_members(party_id, user_id);
CREATE INDEX invites_pending_receiver
    ON invites(receiver, session_type, consumed_at, expires_at);
CREATE INDEX invites_accepted_receiver
    ON invites(receiver, accepted_at, session_type, consumed_at, expires_at);
CREATE INDEX invites_party_follow
    ON invites(party_id, receiver, kind, consumed_at, expires_at);
CREATE TRIGGER scbl_party_member_leave_lobby
AFTER DELETE ON participants
WHEN EXISTS (
    SELECT 1
    FROM game_sessions AS lobby
    WHERE lobby.id = OLD.game_id
      AND (';' || replace(lobby.attributes, ' ', '') || ';') LIKE '%;113=>1;%'
)
AND NOT EXISTS (
    SELECT 1
    FROM participants AS current_participant
    INNER JOIN game_sessions AS current_session
        ON current_session.id = current_participant.game_id
    WHERE current_participant.user_id = OLD.user_id
      AND current_session.destroyed_at IS NULL
      AND (';' || replace(current_session.attributes, ' ', '') || ';') LIKE '%;113=>0;%'
)
AND NOT EXISTS (
    SELECT 1
    FROM invites AS restore
    INNER JOIN party_members AS membership
        ON membership.user_id = OLD.user_id
       AND membership.party_id = restore.party_id
    WHERE restore.receiver = OLD.user_id
      AND restore.kind = 4
      AND restore.accepted_at IS NOT NULL
      AND restore.consumed_at IS NULL
      AND restore.expires_at > CURRENT_TIMESTAMP
)
BEGIN
    DELETE FROM parties WHERE leader_id = OLD.user_id;
    DELETE FROM party_members WHERE user_id = OLD.user_id;
END;
INSERT INTO users(id,username,password) VALUES(105,'Tracking','JaDe!');
-- Preserve the invitation generation namespace; this is a protocol counter, not player data.
INSERT INTO sqlite_sequence(name,seq) VALUES('invites',1000000000);
