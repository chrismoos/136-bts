ALTER TABLE sms_receipts RENAME TO delivery_receipts;
CREATE TABLE sms_receipts (
    original_id INTEGER NOT NULL REFERENCES sms_queue(id),
    acknowledgment_kind INTEGER NOT NULL CHECK(acknowledgment_kind IN (1, 2)),
    receipt_id INTEGER NOT NULL UNIQUE REFERENCES sms_queue(id),
    PRIMARY KEY(original_id, acknowledgment_kind)
) STRICT;
INSERT INTO sms_receipts(original_id, acknowledgment_kind, receipt_id)
SELECT original_id, 1, receipt_id FROM delivery_receipts;
DROP TABLE delivery_receipts;
