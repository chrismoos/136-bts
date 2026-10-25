CREATE TABLE tsar_incoming (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner BLOB NOT NULL DEFAULT (randomblob(16)) CHECK (length(owner) = 16),
    peer_type INTEGER NOT NULL CHECK (peer_type BETWEEN 0 AND 3),
    peer_value INTEGER NOT NULL CHECK (peer_value > 0),
    hlpi INTEGER NOT NULL CHECK (hlpi BETWEEN 0 AND 255),
    tsar_id INTEGER NOT NULL CHECK (tsar_id BETWEEN 0 AND 7),
    protocol_version INTEGER NOT NULL CHECK (protocol_version BETWEEN 0 AND 255),
    context BLOB NOT NULL,
    maximum_events INTEGER NOT NULL CHECK (maximum_events BETWEEN 1 AND 4294967295),
    maximum_context_octets INTEGER NOT NULL CHECK (maximum_context_octets BETWEEN 1 AND 4294967295),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    observed_at_unix_ms INTEGER NOT NULL CHECK (observed_at_unix_ms >= created_at_unix_ms),
    event_count INTEGER NOT NULL DEFAULT 0 CHECK (event_count BETWEEN 0 AND maximum_events),
    application_state INTEGER NOT NULL DEFAULT 0 CHECK (application_state IN (0, 1, 2)),
    application_sms_id INTEGER REFERENCES sms_queue(id),
    reuse_after_unix_ms INTEGER CHECK (reuse_after_unix_ms >= observed_at_unix_ms),
    CHECK (length(context) <= maximum_context_octets),
    CHECK ((application_state = 1 AND application_sms_id IS NOT NULL) OR
           (application_state <> 1 AND application_sms_id IS NULL))
) STRICT;
CREATE UNIQUE INDEX tsar_incoming_active_slot ON tsar_incoming(peer_type, peer_value, hlpi, tsar_id)
    WHERE reuse_after_unix_ms IS NULL;
CREATE TABLE tsar_incoming_events (
    stream_id INTEGER NOT NULL REFERENCES tsar_incoming(id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 4294967294),
    observed_at_unix_ms INTEGER NOT NULL CHECK (observed_at_unix_ms >= 0),
    event_type INTEGER NOT NULL CHECK (event_type BETWEEN 0 AND 4),
    segment BLOB NOT NULL,
    sms_id INTEGER REFERENCES sms_queue(id),
    CHECK ((event_type = 0 AND length(segment) BETWEEN 1 AND 254 AND sms_id IS NULL) OR
           (event_type = 2 AND length(segment) = 0 AND sms_id IS NOT NULL) OR
           (event_type IN (1, 3, 4) AND length(segment) = 0 AND sms_id IS NULL)),
    PRIMARY KEY (stream_id, ordinal)
) STRICT;
