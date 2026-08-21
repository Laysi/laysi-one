-- Five settings were seeded non-editable with an empty value, and the setup
-- wizard's first step told the user to go and set them in 「基礎設定」. Arriving
-- there, none of the five had a control: the screen offered a row that could
-- only ever read 「（尚未設定）」.
--
-- Each of the five is a different problem, so each gets a different answer.
--
-- currency: a single-country product has one functional currency. Seed it and
-- leave it non-editable, because there is nothing to decide.
--
-- fiscal_year: a real decision, so it becomes editable. The value is the
-- starting month, and `Store::fiscal_year_start_month` is what the balance
-- sheet and income statement resolve their default period against, so the
-- field is read rather than merely stored.
--
-- period, doc_number, opening_locked: not settings at all. The first two are
-- derived — the current period from the calendar, the numbering rule from the
-- document numberer — and `get_company_settings` now returns them computed,
-- marked `source = 'derived'`. Opening balances are entered as an ordinary
-- voucher today, and there is no import to link a status row to, so that row
-- is removed rather than left saying 「尚未建立」 with nowhere to go; it comes
-- back with the import.

UPDATE company_setting
   SET value = 'TWD',
       note = '單一國家產品，記帳本位幣固定為新臺幣。'
 WHERE code = 'currency';

UPDATE company_setting
   SET editable = true,
       value = '1',
       note = '會計年度起始月份，填 1 到 12。財務報表的預設期間跟著它走。'
 WHERE code = 'fiscal_year';

DELETE FROM company_setting WHERE code IN ('period', 'doc_number', 'opening_locked');
