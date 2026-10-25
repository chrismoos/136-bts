ALTER TABLE sms_queue RENAME COLUMN deliver_pdu TO sms_pdu;
UPDATE sms_queue SET dedup_key = 'submit:' || dedup_key;
CREATE TABLE sms_receipts (
    original_id INTEGER PRIMARY KEY REFERENCES sms_queue(id),
    receipt_id INTEGER NOT NULL UNIQUE REFERENCES sms_queue(id)
) STRICT;
