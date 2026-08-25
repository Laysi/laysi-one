-- 規章保存每一版本，並移除閱讀確認
--
-- 0004 讓改寫規章清掉 notice_ack。那條路把「誰在哪一天讀過哪一段文字」銷毀，而那
-- 正是勞資雙方事後唯一拿得出來的東西。確認整套移除，改成每一次修訂都留一版，任何
-- 看得到佈告欄的人都讀得到過去的版本。
--
-- 內容從此只住在 notice_version：標題、內文與生效日都是某一版的事實，改了就是新的
-- 一版。notice 只留身分（kind）、廢止日與指向現行版本的 version_no，那個指標由一條
-- deferred 外鍵釘住——建立時要先寫 notice 才寫得了 version，所以不能是立即檢查。
--
-- 生效日與廢止日的先後改由 ops 層驗：兩個值從此不在同一張表，CHECK 寫不出來。

CREATE TABLE notice_version (
    notice_id integer NOT NULL,
    version_no integer NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    effective_on date NOT NULL,
    published_at timestamp with time zone DEFAULT now() NOT NULL,
    published_by text NOT NULL,
    CONSTRAINT notice_version_pkey PRIMARY KEY (notice_id, version_no),
    CONSTRAINT notice_version_notice_id_fkey FOREIGN KEY (notice_id) REFERENCES notice(id) ON DELETE CASCADE,
    CONSTRAINT notice_version_version_no_check CHECK ((version_no > 0)),
    CONSTRAINT notice_version_title_present CHECK ((btrim(title) <> ''::text)),
    CONSTRAINT notice_version_body_present CHECK ((btrim(body) <> ''::text))
);

COMMENT ON TABLE notice_version IS
    '一則佈告欄內容的每一個版本，只增不改。改動標題、內文或生效日就是新的一版。';
COMMENT ON COLUMN notice_version.effective_on IS
    '本版的發布日或施行日。每一版各有各的日期，前一版到下一版施行為止。';

INSERT INTO notice_version
    (notice_id, version_no, title, body, effective_on, published_at, published_by)
SELECT id, 1, title, body, effective_on, created_at, created_by FROM notice;

ALTER TABLE notice ADD COLUMN version_no integer NOT NULL DEFAULT 1;
ALTER TABLE notice ADD CONSTRAINT notice_current_version_fkey
    FOREIGN KEY (id, version_no) REFERENCES notice_version(notice_id, version_no)
    DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE notice DROP CONSTRAINT notice_title_present;
ALTER TABLE notice DROP CONSTRAINT notice_body_present;
ALTER TABLE notice DROP CONSTRAINT notice_window_check;
DROP INDEX notice_window_idx;

ALTER TABLE notice DROP COLUMN title;
ALTER TABLE notice DROP COLUMN body;
ALTER TABLE notice DROP COLUMN effective_on;
ALTER TABLE notice DROP COLUMN updated_at;
ALTER TABLE notice DROP COLUMN updated_by;
ALTER TABLE notice DROP COLUMN created_at;
ALTER TABLE notice DROP COLUMN created_by;

COMMENT ON COLUMN notice.version_no IS
    '現行版本。誰在何時發布這一版由 notice_version 回答，這裡不另存一份。';

DROP TABLE notice_ack;

DELETE FROM mcp_tool_grant WHERE tool_name = 'acknowledge_notice';

UPDATE mcp_tool_grant SET note = '讀佈告欄的公告與規章，含修訂紀錄' WHERE tool_name = 'list_notices';
UPDATE mcp_tool_grant SET note = '讀本人看得到的公告與規章，含修訂紀錄' WHERE tool_name = 'list_my_notices';
UPDATE mcp_tool_grant SET note = '修改佈告欄內容，改動內容會發布新的一版。對 AI 預設關閉。' WHERE tool_name = 'update_notice';

UPDATE permission SET op_names = '{get_my_home,list_my_payslips,list_my_leave_requests,request_leave,list_my_expense_claims,submit_my_expense_claim,attach_my_expense_receipt,punch_clock,list_my_punches,issue_my_api_token,list_my_api_tokens,revoke_my_api_token,submit_my_punch_correction,list_my_punch_corrections,submit_my_overtime_request,list_my_overtime_requests,list_my_notices}' WHERE code = 'staff_self_service';
