CREATE TABLE subscribers_next (
    identity_type INTEGER NOT NULL CHECK(identity_type BETWEEN 0 AND 3),
    identity_value INTEGER NOT NULL CHECK(identity_value > 0 AND (
        (identity_type = 0 AND identity_value < 1048576) OR
        (identity_type = 1 AND identity_value < 16777216) OR
        (identity_type = 2 AND identity_value < 17179869184) OR
        (identity_type = 3 AND identity_value < 1125899906842624))),
    number TEXT NOT NULL CHECK(number <> '' AND number NOT GLOB '*[^0-9]*'),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    PRIMARY KEY(identity_type, identity_value)
) STRICT;
INSERT INTO subscribers_next SELECT * FROM subscribers;
DROP TABLE subscribers;
ALTER TABLE subscribers_next RENAME TO subscribers;
CREATE UNIQUE INDEX subscriber_active_number ON subscribers(number) WHERE enabled = 1;
ALTER TABLE sms_queue ADD COLUMN recipient_number TEXT NOT NULL DEFAULT '';
UPDATE sms_queue SET recipient_number = (SELECT number FROM subscribers s WHERE s.identity_type = recipient_type AND s.identity_value = recipient_value);
CREATE TABLE provisioning_state (id INTEGER PRIMARY KEY CHECK(id = 1), seeded INTEGER NOT NULL CHECK(seeded IN (0, 1))) STRICT;
INSERT INTO provisioning_state VALUES(1, EXISTS(SELECT 1 FROM subscribers));
