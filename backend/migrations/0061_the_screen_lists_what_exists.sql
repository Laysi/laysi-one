-- 「匯入與整合」 stops announcing what is not there.
--
-- Five of the six import sources carried `supported = false` and a note saying
-- which parser had not been written; four of the seven integrations carried
-- `live = false` and a note saying the same thing about a feature. A screen
-- has no place for a feature that is not there, and `einvoice` outlived the
-- module 0031 removed while still claiming 「開立與取號已完成」.
--
-- `integration` goes entirely. Of the three rows that were true, REST and MCP
-- are tautologies — the page is being served over the one and the other has
-- its own screen — and object storage was the only row carrying information,
-- which it got wrong: `live` was a constant `true` while the server treats
-- object storage as optional. 「文件管理」 already answers that question from
-- the connection it actually holds, so nothing replaces the row.
--
-- `import_source` stays: `import_batch.source_code` points at it and
-- `supported` is what stops `preview_import` handing an OFX file to the
-- voucher CSV parser.

DELETE FROM import_source WHERE NOT supported;

DROP TABLE integration;
