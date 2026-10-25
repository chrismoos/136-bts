CREATE TABLE ordinary_sms_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner BLOB NOT NULL DEFAULT (randomblob(16)) CHECK(length(owner) = 16),
    sender_type INTEGER NOT NULL CHECK(sender_type BETWEEN 0 AND 3),
    sender_value INTEGER NOT NULL CHECK(sender_value > 0),
    request BLOB NOT NULL,
    optional_octets BLOB NOT NULL,
    observed_at_unix_ms INTEGER NOT NULL CHECK(observed_at_unix_ms >= 0),
    expires_at_unix_ms INTEGER NOT NULL CHECK(expires_at_unix_ms > observed_at_unix_ms),
    response BLOB,
    settled_at_unix_ms INTEGER,
    CHECK(length(request) BETWEEN 1 AND 260),
    CHECK(length(optional_octets) <= 255),
    CHECK(response IS NULL OR length(response) BETWEEN 1 AND 255),
    CHECK((response IS NULL) = (settled_at_unix_ms IS NULL)),
    CHECK(settled_at_unix_ms IS NULL OR settled_at_unix_ms >= observed_at_unix_ms)
) STRICT;
CREATE INDEX ordinary_sms_request_lookup ON ordinary_sms_requests(sender_type,sender_value,request,optional_octets,id DESC);
CREATE INDEX ordinary_sms_request_expiry ON ordinary_sms_requests(expires_at_unix_ms);
CREATE INDEX ordinary_sms_request_clock ON ordinary_sms_requests(coalesce(settled_at_unix_ms,observed_at_unix_ms));
