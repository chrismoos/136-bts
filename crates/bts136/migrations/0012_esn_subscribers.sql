CREATE TABLE esn_subscribers (
    esn INTEGER PRIMARY KEY CHECK(esn > 0 AND esn <= 4294967295),
    number TEXT NOT NULL CHECK(number <> '' AND number NOT GLOB '*[^0-9]*'),
    identity_type INTEGER,
    identity_value INTEGER,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    CHECK((identity_type IS NULL AND identity_value IS NULL) OR
          (identity_type IS NOT NULL AND identity_value IS NOT NULL AND identity_type BETWEEN 0 AND 3 AND identity_value > 0)),
    FOREIGN KEY(identity_type, identity_value) REFERENCES subscribers(identity_type, identity_value)
) STRICT;
CREATE UNIQUE INDEX esn_active_number ON esn_subscribers(number) WHERE enabled = 1;
CREATE UNIQUE INDEX esn_active_identity ON esn_subscribers(identity_type, identity_value) WHERE enabled = 1;
