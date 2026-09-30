-- One-time download links, for documents too large to travel inside an
-- operation result.
--
-- `get_document` returns content as base64 in JSON, which a screen has to
-- decode in memory. `get_download_link` hands out a link instead;
-- `GET /v1/downloads/{secret}` fetches the object, checks its BLAKE3 against
-- `document_version`, and only then sends it. The screens use it; its MCP
-- switch is seeded off.
--
-- A link opens exactly one version of one document, once, for five minutes.
-- The secret is stored as its BLAKE3 and the holder is an account or an MCP
-- token, never both, as for `upload_batch`; neither has a foreign key, so a
-- deleted account or token leaves an id that matches nothing and refuses the
-- link.

CREATE TABLE download_link (
    id uuid NOT NULL,
    secret_hash text NOT NULL,
    owner_user_id integer,
    owner_token_id integer,
    actor text NOT NULL,
    actor_role text NOT NULL,
    channel text NOT NULL,
    document_version_id integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    CONSTRAINT download_link_pkey PRIMARY KEY (id),
    CONSTRAINT download_link_secret_hash_key UNIQUE (secret_hash),
    CONSTRAINT download_link_document_version_id_fkey FOREIGN KEY (document_version_id) REFERENCES document_version(id) ON DELETE CASCADE,
    CONSTRAINT download_link_one_holder CHECK (((owner_user_id IS NULL) <> (owner_token_id IS NULL))),
    CONSTRAINT download_link_channel_check CHECK ((channel = ANY (ARRAY['ui'::text, 'mcp'::text])))
);

CREATE INDEX download_link_expires_at_idx ON download_link USING btree (expires_at);

COMMENT ON TABLE download_link IS
    '下載連結。連結本身只存 BLAKE3，限用一次；持有者是登入帳號或 MCP Token 其中之一。';
COMMENT ON COLUMN download_link.used_at IS
    '連結被使用的時刻。開始驗證檔案之前寫入，驗證失敗也算用過。';

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_documents', '管理文件', '查看、上傳、下載與驗證文件版本與內容雜湊', '{list_documents,upload_document,get_document,request_upload_link,list_uploads,get_download_link}', 85)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('get_download_link', false, 'read_only', '取得一次性的下載連結，檔案驗過內容雜湊才送出。對 AI 預設關閉。', '2026-09-30 00:00:00+00')
ON CONFLICT (tool_name) DO UPDATE
    SET enabled = EXCLUDED.enabled, risk = EXCLUDED.risk,
        note = EXCLUDED.note, updated_at = EXCLUDED.updated_at;
