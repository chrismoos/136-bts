CREATE TABLE tsar_incoming_responses (
    stream_id INTEGER NOT NULL,
    event_ordinal INTEGER NOT NULL,
    response BLOB NOT NULL CHECK (length(response) BETWEEN 1 AND 254),
    awaits_application INTEGER NOT NULL CHECK (awaits_application IN (0, 1)),
    outcome INTEGER NOT NULL DEFAULT 0 CHECK (outcome BETWEEN 0 AND 4),
    settled_at_unix_ms INTEGER CHECK (settled_at_unix_ms >= 0),
    PRIMARY KEY (stream_id, event_ordinal),
    FOREIGN KEY (stream_id, event_ordinal) REFERENCES tsar_incoming_events(stream_id, ordinal),
    CHECK ((outcome = 0 AND settled_at_unix_ms IS NULL) OR
           (outcome <> 0 AND settled_at_unix_ms IS NOT NULL))
) STRICT;
