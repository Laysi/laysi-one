-- A separate channel for shipped regulation versions.
--
-- Amended rates are data, not schema. Putting them in migrations/ would mean
-- a rate correction stands in the way the next time that directory is
-- squashed the way 0001 squashed sixty-three files, and a squash cannot
-- preserve "this installation already had that version and deleted it".
-- regulation-updates/ is applied by its own runner and tracked here, so the
-- two sets never share a version number.
--
-- Keyed by file name rather than a number: the files are dated, they are
-- independent of each other, and a gap means a release shipped no amendment.

CREATE TABLE reg_update (
    file_name  text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT reg_update_pkey PRIMARY KEY (file_name)
);

-- reg_ruleset.checksum was an FNV-1a of the payload computed on write and
-- read by nothing: regulations() does not select it and resolve() never sees
-- it. NOT NULL on a dead column is the one thing standing between a shipped
-- amendment and a plain INSERT, so it goes rather than being faked in SQL.
ALTER TABLE reg_ruleset DROP COLUMN checksum;
