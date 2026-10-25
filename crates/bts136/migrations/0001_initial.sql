CREATE TABLE subscribers (
    identity_type INTEGER NOT NULL CHECK (identity_type BETWEEN 0 AND 3),
    identity_value INTEGER NOT NULL CHECK (
        identity_value > 0 AND (
            (identity_type = 0 AND identity_value < 1048576) OR
            (identity_type = 1 AND identity_value < 16777216) OR
            (identity_type = 2 AND identity_value < 17179869184) OR
            (identity_type = 3 AND identity_value < 1125899906842624)
        )
    ),
    number TEXT NOT NULL UNIQUE CHECK (number <> '' AND number NOT GLOB '*[^0-9]*'),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    PRIMARY KEY (identity_type, identity_value)
) STRICT;
CREATE TABLE sms_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    dedup_key TEXT NOT NULL UNIQUE CHECK (dedup_key <> ''),
    sender_type INTEGER NOT NULL,
    sender_value INTEGER NOT NULL,
    recipient_type INTEGER NOT NULL,
    recipient_value INTEGER NOT NULL,
    sender_number TEXT NOT NULL,
    deliver_pdu BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    state INTEGER NOT NULL DEFAULT 0 CHECK (state IN (0, 1, 2)),
    FOREIGN KEY (sender_type, sender_value) REFERENCES subscribers(identity_type, identity_value),
    FOREIGN KEY (recipient_type, recipient_value) REFERENCES subscribers(identity_type, identity_value)
) STRICT;
CREATE INDEX sms_pending_recipient ON sms_queue(recipient_type, recipient_value, state, id);
