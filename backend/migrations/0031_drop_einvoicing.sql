-- Issuing a uniform invoice is the value-added centre's job. This product does
-- not do it and will not: the upload state machine that stopped at `queued`
-- was not progress waiting to happen, it was a line that was never going to be
-- connected. Removing it takes out three operations, one screen and two
-- tables.
--
-- The VAT return is unaffected. Form 401 has always been derived from posted
-- entries — output tax is the period movement of 2211 — and never summed
-- `einvoice`. What changes is where 2211 gets its entries: `create_ar_invoice`
-- now splits the output tax out itself, which makes it the only place output
-- tax is recorded.

DROP TABLE IF EXISTS einvoice;
DROP TABLE IF EXISTS einvoice_track;

-- `document.link_kind` could point at an e-invoice. Existing rows are moved to
-- `other` before the CHECK narrows, so the constraint can be validated.
UPDATE document SET link_type = 'other', link_ref = '' WHERE link_type = 'einvoice';

ALTER TABLE document DROP CONSTRAINT IF EXISTS document_link_type_check;
ALTER TABLE document ADD CONSTRAINT document_link_type_kinds CHECK (link_type IN (
    '','voucher','ap_bill','ar_invoice','expense_claim','bank_account',
    'import_batch','period','other'));

DELETE FROM mcp_tool_grant
 WHERE tool_name IN ('list_einvoices', 'issue_einvoice', 'retry_einvoice_upload');

DELETE FROM integration WHERE code = 'einvoice';

-- The permission survives because creating a receivable is still a job someone
-- has to be allowed to do; only its name and its single operation change.
UPDATE permission
   SET op_names = '{create_ar_invoice}',
       label = '開立應收單',
       note = '建立應收單據並產生待過帳傳票；營業稅申報另由 manage_tax 管理'
 WHERE code = 'issue_invoice';
