ALTER TABLE sms_queue ADD COLUMN relay_accepted INTEGER NOT NULL DEFAULT 0 CHECK(relay_accepted IN (0, 1));
