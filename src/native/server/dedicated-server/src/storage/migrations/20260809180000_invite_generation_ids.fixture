-- Invitation IDs cross the process boundary and can outlive the database row
-- that originally allocated them.  SQLite's implicit rowid may be reused once
-- rows are deleted, so rebuild the table with an explicit AUTOINCREMENT key.
CREATE TABLE invites_with_generation (
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
);

INSERT INTO invites_with_generation (
    id,
    sender,
    receiver,
    created,
    session_type,
    session_id,
    delivered_at,
    consumed_at,
    expires_at,
    kind,
    accepted_at,
    delivery_count,
    party_id
)
SELECT
    rowid,
    sender,
    receiver,
    created,
    session_type,
    session_id,
    delivered_at,
    consumed_at,
    expires_at,
    kind,
    accepted_at,
    delivery_count,
    party_id
FROM invites;

DROP TABLE invites;
ALTER TABLE invites_with_generation RENAME TO invites;

CREATE INDEX invites_pending_receiver
    ON invites(receiver, session_type, consumed_at, expires_at);
CREATE INDEX invites_accepted_receiver
    ON invites(receiver, accepted_at, session_type, consumed_at, expires_at);
CREATE INDEX invites_party_follow
    ON invites(party_id, receiver, kind, consumed_at, expires_at);

-- A pre-migration table may be empty and therefore has no surviving rowid high
-- water mark.  Move the first post-migration generation far away from legacy
-- low IDs while remaining within the Uplay invitation-id u32 boundary.
UPDATE sqlite_sequence
SET seq = MAX(seq, 1000000000)
WHERE name = 'invites';
INSERT INTO sqlite_sequence(name, seq)
SELECT 'invites', 1000000000
WHERE NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'invites');
