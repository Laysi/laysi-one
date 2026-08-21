-- A month-end reimbursement credited 2101 without leaving a payables entry.
--
-- The payables screen lists `ap_bill` and reconciles the total against the
-- 2101 balance in the general ledger. Approving an expense claim whose pay
-- method is `reimburse` credits 2101 from the expense voucher, so every such
-- claim moved the ledger and not the detail: the screen then reported a
-- difference of exactly that claim, permanently. Nothing on any screen could
-- close it either, because the bill form always creates a voucher of its own
-- and would have doubled the balance instead.
--
-- The bill also carries the claim into a payment batch, which is the only way
-- the money leaves. Without it the amount was owed to the claimant with no
-- way to pay it.
--
-- Approvals from here on write the bill in the same transaction as the
-- voucher. This backfills the claims already approved. `bill_no` is the claim
-- number, so the payables row and the expense claim are the same document
-- under the same name; a manually entered bill that already took that number
-- or that voucher is left alone.

INSERT INTO ap_bill
    (bill_no, vendor, memo, billed_on, due_on, amount, origin_voucher_no)
SELECT c.claim_no,
       c.claimant,
       v.memo,
       v.voucher_date,
       (date_trunc('month', v.voucher_date)
           + interval '1 month' - interval '1 day')::date,
       c.amount,
       v.no
  FROM expense_claim c
  JOIN voucher v ON v.no = c.voucher_no
 WHERE c.status = 'approved'
   AND c.pay_method = 'reimburse'
   AND v.status <> 'void'
   AND NOT EXISTS (SELECT 1 FROM ap_bill b WHERE b.origin_voucher_no = v.no)
   AND NOT EXISTS (SELECT 1 FROM ap_bill b WHERE b.bill_no = c.claim_no);
