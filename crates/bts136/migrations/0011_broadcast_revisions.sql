CREATE TABLE broadcast_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id > 0),
    owner BLOB NOT NULL UNIQUE DEFAULT (randomblob(16)) CHECK(length(owner) = 16),
    band INTEGER NOT NULL CHECK(band BETWEEN 0 AND 1),
    channel INTEGER NOT NULL CHECK(channel BETWEEN 1 AND 2047),
    contents BLOB NOT NULL CHECK(length(contents) BETWEEN 1 AND 65535),
    fbcch INTEGER NOT NULL CHECK(fbcch BETWEEN 0 AND 1),
    ebcch INTEGER NOT NULL CHECK(ebcch BETWEEN 0 AND 1),
    notification INTEGER NOT NULL CHECK(notification BETWEEN 0 AND 1),
    prepared_at_unix_ms INTEGER NOT NULL CHECK(prepared_at_unix_ms >= 0),
    confirmed_at_unix_ms INTEGER CHECK(confirmed_at_unix_ms >= prepared_at_unix_ms),
    CHECK((band = 0 AND (channel BETWEEN 1 AND 799 OR channel BETWEEN 991 AND 1023)) OR (band = 1 AND channel BETWEEN 2 AND 1998))
) STRICT;
CREATE UNIQUE INDEX broadcast_pending ON broadcast_revisions(band,channel) WHERE confirmed_at_unix_ms IS NULL;
CREATE INDEX broadcast_confirmed ON broadcast_revisions(band,channel,id DESC) WHERE confirmed_at_unix_ms IS NOT NULL;
CREATE INDEX broadcast_clock ON broadcast_revisions(coalesce(confirmed_at_unix_ms,prepared_at_unix_ms));
