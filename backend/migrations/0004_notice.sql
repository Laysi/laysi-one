-- 佈告欄與規章制度
--
-- One table holds both. An announcement and a company rule differ in what
-- they ask of the reader, not in what they store, and `kind = 'policy'` is
-- the only thing that requires an acknowledgement — so there is no separate
-- flag that could disagree with the kind.
--
-- Editing a policy's body clears its acknowledgements. A confirmation made
-- against an earlier text is not a confirmation of the current one, and the
-- roster of who has not confirmed yet has to stay true.

CREATE TABLE notice (
    id serial,
    kind text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    effective_on date NOT NULL,
    expires_on date,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_by text NOT NULL,
    CONSTRAINT notice_pkey PRIMARY KEY (id),
    CONSTRAINT notice_kind_check CHECK ((kind = ANY (ARRAY['announcement'::text, 'policy'::text]))),
    CONSTRAINT notice_title_present CHECK ((btrim(title) <> ''::text)),
    CONSTRAINT notice_body_present CHECK ((btrim(body) <> ''::text)),
    CONSTRAINT notice_window_check CHECK (((expires_on IS NULL) OR (expires_on >= effective_on)))
);
CREATE INDEX notice_window_idx ON public.notice USING btree (effective_on DESC, id DESC);

COMMENT ON TABLE notice IS
    '佈告欄的一則內容。announcement 是公告，policy 是規章，只有規章要求閱讀確認。';
COMMENT ON COLUMN notice.effective_on IS
    '公告的發布日、規章的生效日。這一天之前不出現在員工端。';
COMMENT ON COLUMN notice.expires_on IS
    '公告的下架日、規章的廢止日。空值表示持續有效。';

CREATE TABLE notice_ack (
    notice_id integer NOT NULL,
    employee_id integer NOT NULL,
    acked_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT notice_ack_pkey PRIMARY KEY (notice_id, employee_id),
    CONSTRAINT notice_ack_notice_id_fkey FOREIGN KEY (notice_id) REFERENCES notice(id) ON DELETE CASCADE,
    CONSTRAINT notice_ack_employee_id_fkey FOREIGN KEY (employee_id) REFERENCES employee(id) ON DELETE CASCADE
);

COMMENT ON TABLE notice_ack IS
    '員工本人對一則規章的閱讀確認。規章內容一經修改就整批刪除，確認才不會指向已經改掉的文字。';

UPDATE permission SET op_names = '{get_hr_dashboard,list_employees,list_leave_requests,get_insurance_summary,compute_insurance,get_hr_compliance,list_attendance_punches,list_punch_corrections,list_overtime_requests,list_notices}' WHERE code = 'view_hr';

UPDATE permission SET op_names = '{decide_leave_request,create_employee,update_employee,correct_punch,decide_punch_correction,decide_overtime_request,create_notice,update_notice}' WHERE code = 'manage_hr';

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests,list_my_notices,acknowledge_notice}' WHERE code = 'staff_self_service';

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('list_notices', true, 'read_only', '讀佈告欄的公告與規章，含未確認名單', now()),
    ('create_notice', false, 'needs_confirm', '發布公告或規章，對 AI 預設關閉。', now()),
    ('update_notice', false, 'needs_confirm', '修改或下架佈告欄內容，對 AI 預設關閉。', now()),
    ('list_my_notices', true, 'read_only', '讀本人看得到的公告與規章', now()),
    ('acknowledge_notice', false, 'needs_confirm', '確認已閱讀規章。這一下要由本人按，對 AI 一律關閉。', now());
