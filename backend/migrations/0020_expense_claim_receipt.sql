-- Laysi One: employees attach the receipt to their own claim
--
-- `submit_my_expense_claim` deliberately takes no document number: staff hold
-- no document permission, so any number they could type would be somebody
-- else's, and the reply would disclose its name. That reasoning rules out
-- pointing at an existing document. It does not rule out sending a new file,
-- which is what a receipt actually is, so `attach_my_expense_receipt` takes
-- bytes rather than a reference.
--
-- Ownership needs a key that is not a name. `expense_claim.claimant` is a
-- display name and two people can share one, which is tolerable for the read
-- side that already uses it but not for a write. The new column records which
-- employee filed the claim through self-service and goes into the WHERE clause
-- of the attachment, so somebody else's claim number is a missing row rather
-- than a failed check. Claims filed by accounting on another person's behalf
-- leave it NULL: those represent a person without belonging to their account.

ALTER TABLE expense_claim
    ADD COLUMN employee_id int REFERENCES employee(id) ON DELETE SET NULL;

COMMENT ON COLUMN expense_claim.employee_id IS
    '自助送出時的申請人。NULL 表示由會計代為建立，申請人只有姓名。';

CREATE INDEX expense_claim_employee_idx ON expense_claim (employee_id, status);

-- Permissions

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token}' WHERE code = 'staff_self_service';

UPDATE permission SET note = '只回目前登入帳號所連結員工本人的首頁、薪資、休假、打卡、報支進度與自己核發的 MCP Token，並可為本人的報支單附上收據' WHERE code = 'staff_self_service';

-- MCP tool grants
--
-- Off, like the other self-service writes. An assistant that can put files
-- into object storage under someone's name is a larger surface than the claim
-- itself, and what lands in the bucket is never deleted.

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('attach_my_expense_receipt', false, 'needs_confirm',
     '為本人送出的報支單附上收據檔案。寫入物件儲存且無法刪除，預設關閉。')
ON CONFLICT (tool_name) DO NOTHING;
