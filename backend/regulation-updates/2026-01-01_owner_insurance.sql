-- owner_insurance 2026.1，全民健康保險法第 27 條第 1 款第 1 目；勞工退休金條例第 7 條、第 14 條
-- 由 scripts/regulation-sql.ts 從 regulation/2026/owner_insurance.toml 產出，不要手改。
--
-- DO NOTHING：這一版已經由管理員自己建過就不動它。

INSERT INTO reg_ruleset (domain, version, effective_from, effective_to, source_note, payload)
VALUES (
    'owner_insurance',
    '2026.1',
    '2026-01-01',
    NULL,
    '全民健康保險法第 27 條第 1 款第 1 目；勞工退休金條例第 7 條、第 14 條',
    '{"health_employer_share":"0","health_employee_share":"1.00","health_government_share":"0","pension_employer_rate":"0"}'::jsonb
)
ON CONFLICT (domain, version) DO NOTHING;
