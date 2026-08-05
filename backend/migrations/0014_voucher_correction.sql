-- Laysi One: voiding and reversing posted vouchers
--
-- Voiding says the original voucher should never have existed; a reversal
-- keeps the original transaction and creates a new voucher with the opposite
-- debits and credits on a given date. Both change the ledger, so the human
-- permission follows post_voucher: whoever may post a voucher may correct a
-- posted one.
--
-- Both MCP tools are off by default. A model can help identify a suspected
-- error, but must not decide on its own that a posted voucher should be
-- invalidated, or when to correct it with a reversing entry.

-- Reversal link
--
-- The link lives only on the new reversing voucher. "Which voucher reversed
-- this one" is found by following that unique link backwards, so the posted
-- original never has to be UPDATEd and the immutability trigger does not
-- have to be relaxed.

ALTER TABLE voucher
    ADD COLUMN reverses uuid REFERENCES voucher(id),
    ADD CONSTRAINT voucher_reverses_once UNIQUE (reverses),
    ADD CONSTRAINT voucher_cannot_reverse_itself CHECK (reverses IS NULL OR reverses <> id);

COMMENT ON COLUMN voucher.reverses IS
    '這張傳票沖銷的原傳票；NULL 代表不是沖銷傳票。UNIQUE 保證同一張原傳票只會被沖銷一次。';

-- A reversal is its own voucher origin and must not be folded into manual,
-- or a review could not see why the voucher exists.
ALTER TABLE voucher DROP CONSTRAINT voucher_source_check;
ALTER TABLE voucher ADD CONSTRAINT voucher_source_check
    CHECK (source IN (
        'manual','ai_draft','import','payroll','invoice','depreciation','expense','reversal'
    ));

-- AI tool grants

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note) VALUES
    ('void_voucher', false, 'high_risk',
     '作廢會讓一張已過帳傳票退出總帳與報表；AI 預設不得自行判定原交易不存在。'),
    ('reverse_voucher', false, 'high_risk',
     '沖銷會立即過帳一張反向傳票；AI 預設不得自行決定更正日期與正式總帳分錄。');

-- Human permissions
--
-- These follow the posting permission rather than a new group that might
-- end up granted to nobody. That also preserves deny-by-default: only the
-- owner, internal accounting and the external accountant, who can already
-- post, may correct.

UPDATE permission
   SET op_names = '{post_voucher,void_voucher,reverse_voucher}',
       note = '過帳後不可修改；不應存在的傳票可作廢，確實發生但記錯的交易以反向傳票更正'
 WHERE code = 'post_voucher';
