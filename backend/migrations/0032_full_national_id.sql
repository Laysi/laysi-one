-- The masked national ID was a dead end. Labour insurance filing and the
-- withholding statement both need the full number, so keeping only a mask
-- meant HR maintained a second copy in a spreadsheet: the personal data ended
-- up outside the system, where nothing logs who read it.
--
-- The column now holds the full number, validated on the way in (one letter,
-- nine digits, check digit). Masking moves to the screen, which is where a
-- display decision belongs, and `redact_for_audit` keeps `id_no` out of
-- `ai_audit_log` by field name.

COMMENT ON COLUMN employee.id_no IS
    '身分證字號，勞保申報與扣繳憑單都要用到。寫入時驗檢查碼；畫面預設遮罩顯示，稽核紀錄不留此欄的值。';
