CREATE TEMP TABLE tsar_outgoing_previous_sequence(seq INTEGER NOT NULL);
INSERT INTO tsar_outgoing_previous_sequence SELECT COALESCE(MAX(seq),0) FROM sqlite_sequence WHERE name='tsar_outgoing';
CREATE TABLE tsar_outgoing_next (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner BLOB NOT NULL DEFAULT (randomblob(16)) CHECK(length(owner) = 16),
    sms_id INTEGER NOT NULL UNIQUE REFERENCES sms_queue(id),
    peer_type INTEGER NOT NULL CHECK(peer_type BETWEEN 0 AND 3),
    peer_value INTEGER NOT NULL CHECK(peer_value > 0),
    hlpi INTEGER NOT NULL CHECK(hlpi BETWEEN 0 AND 255),
    tsar_id INTEGER NOT NULL CHECK(tsar_id BETWEEN 0 AND 7),
    protocol_version INTEGER NOT NULL CHECK(protocol_version BETWEEN 0 AND 255),
    payload_budget INTEGER NOT NULL CHECK(payload_budget BETWEEN 1 AND 254),
    message BLOB NOT NULL,
    context BLOB NOT NULL,
    maximum_context_octets INTEGER NOT NULL CHECK(maximum_context_octets BETWEEN 1 AND 4294967295),
    maximum_events INTEGER NOT NULL CHECK(maximum_events BETWEEN 1 AND 4294967295),
    created_at_unix_ms INTEGER NOT NULL CHECK(created_at_unix_ms >= 0),
    observed_at_unix_ms INTEGER NOT NULL CHECK(observed_at_unix_ms >= created_at_unix_ms),
    event_count INTEGER NOT NULL DEFAULT 0 CHECK(event_count BETWEEN 0 AND maximum_events),
    retired_at_unix_ms INTEGER CHECK(retired_at_unix_ms >= observed_at_unix_ms),
    reuse_after_unix_ms INTEGER CHECK(reuse_after_unix_ms > retired_at_unix_ms),
    CHECK(length(context) <= maximum_context_octets),
    CHECK((retired_at_unix_ms IS NULL AND reuse_after_unix_ms IS NULL) OR
          (retired_at_unix_ms IS NOT NULL AND reuse_after_unix_ms IS NOT NULL))
) STRICT;
INSERT INTO tsar_outgoing_next(id,owner,sms_id,peer_type,peer_value,hlpi,tsar_id,protocol_version,payload_budget,message,context,maximum_context_octets,maximum_events,created_at_unix_ms,observed_at_unix_ms,event_count) SELECT id,owner,sms_id,peer_type,peer_value,hlpi,tsar_id,protocol_version,payload_budget,message,context,maximum_context_octets,maximum_events,created_at_unix_ms,observed_at_unix_ms,event_count FROM tsar_outgoing;
DROP TABLE tsar_outgoing;
ALTER TABLE tsar_outgoing_next RENAME TO tsar_outgoing;
UPDATE sqlite_sequence SET seq=MAX(seq,(SELECT seq FROM tsar_outgoing_previous_sequence)) WHERE name='tsar_outgoing';
INSERT INTO sqlite_sequence(name,seq) SELECT 'tsar_outgoing',seq FROM tsar_outgoing_previous_sequence WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='tsar_outgoing');
DROP TABLE tsar_outgoing_previous_sequence;
CREATE UNIQUE INDEX tsar_outgoing_active_slot ON tsar_outgoing(peer_type,peer_value,hlpi,tsar_id) WHERE retired_at_unix_ms IS NULL;
CREATE INDEX tsar_outgoing_active_recovery ON tsar_outgoing(id) WHERE retired_at_unix_ms IS NULL;
CREATE INDEX tsar_outgoing_guarded_slot ON tsar_outgoing(peer_type,peer_value,hlpi,tsar_id,reuse_after_unix_ms);
ALTER TABLE tsar_incoming ADD COLUMN retired_at_unix_ms INTEGER CHECK(retired_at_unix_ms >= observed_at_unix_ms);
