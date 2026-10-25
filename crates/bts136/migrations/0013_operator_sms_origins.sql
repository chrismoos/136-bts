CREATE TABLE sms_queue_next (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    dedup_key TEXT NOT NULL UNIQUE CHECK(dedup_key <> ''),
    sender_type INTEGER,
    sender_value INTEGER,
    recipient_type INTEGER NOT NULL,
    recipient_value INTEGER NOT NULL,
    sender_number TEXT NOT NULL,
    sms_pdu BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL CHECK(created_at_unix_ms >= 0),
    state INTEGER NOT NULL DEFAULT 0 CHECK(state IN (0,1,2)),
    relay_accepted INTEGER NOT NULL DEFAULT 0 CHECK(relay_accepted IN (0,1)),
    recipient_number TEXT NOT NULL DEFAULT '',
    CHECK((sender_type IS NULL AND sender_value IS NULL) OR
          (sender_type IS NOT NULL AND sender_value IS NOT NULL)),
    FOREIGN KEY(sender_type,sender_value) REFERENCES subscribers(identity_type,identity_value),
    FOREIGN KEY(recipient_type,recipient_value) REFERENCES subscribers(identity_type,identity_value)
) STRICT;
INSERT INTO sms_queue_next(id,dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,state,relay_accepted,recipient_number)
SELECT id,dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,state,relay_accepted,recipient_number FROM sms_queue;
DROP TABLE sms_queue;
ALTER TABLE sms_queue_next RENAME TO sms_queue;
CREATE INDEX sms_pending_recipient ON sms_queue(recipient_type,recipient_value,state,id);
