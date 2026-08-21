-- Two permission notes 0037 missed, and one label that stopped being true.
--
-- 0011 narrowed `view_payroll` to `get_payroll_summary` alone — the per-person
-- figures moved to `view_payroll_details` — but 0022 then restored the older
-- note while renaming 老闆 to 負責人, so the row on 「角色與核准」 has since been
-- labelled 「看薪資明細」 while granting only the company-wide summary. The label
-- and the note both describe the wrong permission, and the note names a
-- function while doing it.

UPDATE permission
   SET label = '看薪資彙總',
       note  = '全公司薪資總額與總帳對帳，不含任何個人金額'
 WHERE code = 'view_payroll';

UPDATE permission
   SET note = '查看、上傳與驗證文件版本與內容雜湊'
 WHERE code = 'manage_documents';
