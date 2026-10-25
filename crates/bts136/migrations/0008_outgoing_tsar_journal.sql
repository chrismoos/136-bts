CREATE TABLE tsar_outgoing (
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
    CHECK(length(context) <= maximum_context_octets),
    UNIQUE(peer_type, peer_value, hlpi, tsar_id)
) STRICT;
CREATE TABLE tsar_outgoing_segments (
    stream_id INTEGER NOT NULL REFERENCES tsar_outgoing(id),
    sequence INTEGER NOT NULL CHECK(sequence BETWEEN 0 AND 255),
    payload BLOB NOT NULL CHECK(length(payload) BETWEEN 3 AND 254),
    PRIMARY KEY(stream_id, sequence)
) STRICT;
CREATE TABLE tsar_outgoing_events (
    stream_id INTEGER NOT NULL REFERENCES tsar_outgoing(id),
    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 4294967294),
    observed_at_unix_ms INTEGER NOT NULL CHECK(observed_at_unix_ms >= 0),
    event BLOB NOT NULL CHECK(length(event) BETWEEN 1 AND 4096),
    PRIMARY KEY(stream_id, ordinal)
) STRICT;
