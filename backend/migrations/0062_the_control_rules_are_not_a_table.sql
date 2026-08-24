-- The two stored control rules move into the operation that prints them.
--
-- `security_policy` had no write path and one reader. 0006 gave it a table so
-- that `enforced` could tell a stated rule from an applied one, but 0035
-- removed the last row that was merely stated: both survivors are enforced,
-- so the column no longer separates anything and the table is two constants
-- in six columns. Six migrations rewrote their wording without any of them
-- changing what the system does.
--
-- 「安全與營運」 already builds its other two rules in `ops/security.rs` from
-- the switches and the approval thresholds themselves. The four rules now
-- come from one place, and the two fixed ones sit beside the derived ones
-- where a reader can see that neither is a description of the other.

DROP TABLE security_policy;
