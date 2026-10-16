-- The release manifest the licence server released to this installation, and
-- the two operations that act on it.
--
-- 0001 is applied and frozen, so everything here is additive.
--
-- The column holds the text of a signed manifest, byte for byte, because an
-- update request carries it unchanged and the host-side updater verifies the
-- same bytes again. It is a copy of something signed, not a setting: nothing
-- reads a version out of it without verifying it first, so a hand-edited row
-- offers nothing. The empty string means nothing is released.
ALTER TABLE license_state
    ADD COLUMN release_manifest text DEFAULT ''::text NOT NULL;
COMMENT ON COLUMN license_state.release_manifest IS
    '授權伺服器放行給本安裝的版本清單（簽章全文）；空字串表示沒有放行。每次成功的檢查整份換掉，讀取時重新驗章。';

-- `get_update_ticket` hands the host-side updater a request and
-- `get_update_result` reads its answer. Both belong with the licence: the
-- manifest arrives with it, and the administrator who maintains one maintains
-- the other.
INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_license', '授權與系統更新', '查看本安裝的授權、被授權單位與經銷商，以及可安裝的新版本；輸入啟動金鑰、重新驗證與執行更新限系統管理員', '{get_license,activate_license,get_update_ticket,get_update_result}', 57)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

-- Both closed to AI. The update is started from the screen only: the request
-- directory is mounted into the api container and nowhere else, and a model
-- must not decide when the system goes down.
INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('get_update_ticket', false, 'admin_only', '要求更新至已放行的版本。只能由系統管理員在「安全與營運」執行，無法對 AI 開啟。', '2026-10-13 00:00:00+00'),
    ('get_update_result', false, 'admin_only', '讀取一次更新的結果。對 AI 預設關閉。', '2026-10-13 00:00:00+00')
ON CONFLICT (tool_name) DO UPDATE
    SET enabled = EXCLUDED.enabled, risk = EXCLUDED.risk,
        note = EXCLUDED.note, updated_at = EXCLUDED.updated_at;
