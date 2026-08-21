-- Two rows on 「安全與營運」 stated something the system does not do, next to a
-- tag reading 「未啟用」. A rule that is both described in detail and marked as
-- not in force is worse than no row: the reader has to decide which half to
-- believe.
--
-- two_factor said 「全員強制（負責人、出納含裝置綁定）」. Neither exists, and the
-- row was the only place saying it should. A screen has no place for a feature
-- that is not there, so the row goes; it comes back the day the feature does.
--
-- audit_retention said 「所有動作保留 10 年」 while the same page reported the
-- actual number of rows. Nothing deletes audit rows and nothing enforces a
-- retention period, so the retention claim goes and the row states what does
-- happen — which is real, and is enforced by dispatch on every call.

DELETE FROM security_policy WHERE code = 'two_factor';

UPDATE security_policy
   SET value = '每一次呼叫都留下紀錄，含被擋下的',
       detail = '唯讀查詢與被權限擋下的呼叫同樣寫入。',
       enforced = true
 WHERE code = 'audit_retention';
