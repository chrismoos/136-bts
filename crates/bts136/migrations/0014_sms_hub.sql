CREATE TABLE sms_queue_next (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    dedup_key TEXT NOT NULL UNIQUE CHECK(dedup_key <> ''),
    sender_type INTEGER,
    sender_value INTEGER,
    recipient_type INTEGER,
    recipient_value INTEGER,
    sender_number TEXT NOT NULL,
    sms_pdu BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL CHECK(created_at_unix_ms >= 0),
    state INTEGER NOT NULL DEFAULT 0 CHECK(state IN (0,1,2)),
    relay_accepted INTEGER NOT NULL DEFAULT 0 CHECK(relay_accepted IN (0,1)),
    recipient_number TEXT NOT NULL DEFAULT '',
    CHECK((sender_type IS NULL AND sender_value IS NULL) OR
          (sender_type IS NOT NULL AND sender_value IS NOT NULL)),
    CHECK((recipient_type IS NULL AND recipient_value IS NULL) OR
          (recipient_type IS NOT NULL AND recipient_value IS NOT NULL)),
    FOREIGN KEY(sender_type,sender_value) REFERENCES subscribers(identity_type,identity_value),
    FOREIGN KEY(recipient_type,recipient_value) REFERENCES subscribers(identity_type,identity_value)
) STRICT;
INSERT INTO sms_queue_next(id,dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,state,relay_accepted,recipient_number)
SELECT id,dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,state,relay_accepted,recipient_number FROM sms_queue;
DROP TABLE sms_queue;
ALTER TABLE sms_queue_next RENAME TO sms_queue;
CREATE INDEX sms_pending_recipient ON sms_queue(recipient_type,recipient_value,state,id);

CREATE TABLE sms_hub_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sms_id INTEGER REFERENCES sms_queue(id),
    pdu BLOB NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','sending','accepted','delivered','failed','expired')),
    peer_id TEXT,
    peer TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    detail TEXT NOT NULL DEFAULT 'Queued for hub',
    receipt_inbox_id INTEGER UNIQUE REFERENCES sms_hub_inbox(id)
) STRICT;
CREATE UNIQUE INDEX sms_hub_submission ON sms_hub_outbox(sms_id) WHERE receipt_inbox_id IS NULL;
CREATE INDEX sms_hub_due ON sms_hub_outbox(state,next_attempt_ms,id);
CREATE TABLE sms_hub_inbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    peer TEXT NOT NULL,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    reference INTEGER,
    recipient_type INTEGER NOT NULL,
    recipient_value INTEGER NOT NULL,
    pdu BLOB NOT NULL,
    sms_id INTEGER REFERENCES sms_queue(id),
    request_receipt INTEGER NOT NULL CHECK(request_receipt BETWEEN 0 AND 2),
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    failed INTEGER NOT NULL DEFAULT 0 CHECK(failed IN (0,1))
) STRICT;
CREATE INDEX sms_hub_inbox_lookup ON sms_hub_inbox(peer,source,destination,reference);
CREATE TABLE sms_hub_parts (
    peer TEXT NOT NULL,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    reference INTEGER NOT NULL,
    width INTEGER NOT NULL,
    coding INTEGER NOT NULL,
    total INTEGER NOT NULL,
    part INTEGER NOT NULL,
    data BLOB NOT NULL,
    inbox_id INTEGER NOT NULL UNIQUE REFERENCES sms_hub_inbox(id),
    PRIMARY KEY(peer,source,destination,reference,width,coding,total,part)
) STRICT;
