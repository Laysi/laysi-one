-- Files that arrive through an upload link rather than inside an operation.
--
-- An operation input carries a file as base64, which a model has to write out
-- token by token: past a few tens of kilobytes it cannot, and a screen upload
-- stops at the 25 MB the JSON body allows. `request_upload_link` hands out a
-- short-lived link instead; each file posted to it is streamed into the
-- bucket under `staging/` and recorded here. `upload_document` then takes the
-- staged file by id and files it as a document.
--
-- Staging first, because a document cannot be deleted once it is filed, and
-- a model has to read a file before it knows which record it belongs to. A
-- staged file nobody takes is removed a day later.
--
-- The link secret is stored as its BLAKE3, like `api_token`. The holder is an
-- account (a person on screen) or an MCP token, never both. Neither has a
-- foreign key, for the reason 0012 gives for `api_token.owner_user_id`: a
-- deleted account or token has to leave an id that matches nothing, which
-- refuses the link, rather than a NULL that would match no rule at all.

CREATE TABLE upload_batch (
    id uuid NOT NULL,
    secret_hash text NOT NULL,
    owner_user_id integer,
    owner_token_id integer,
    actor text NOT NULL,
    actor_role text NOT NULL,
    channel text NOT NULL,
    max_files integer NOT NULL,
    used_files integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    CONSTRAINT upload_batch_pkey PRIMARY KEY (id),
    CONSTRAINT upload_batch_secret_hash_key UNIQUE (secret_hash),
    CONSTRAINT upload_batch_one_holder CHECK (((owner_user_id IS NULL) <> (owner_token_id IS NULL))),
    CONSTRAINT upload_batch_channel_check CHECK ((channel = ANY (ARRAY['ui'::text, 'mcp'::text]))),
    CONSTRAINT upload_batch_files_check CHECK (((max_files > 0) AND (used_files >= 0) AND (used_files <= max_files)))
);

COMMENT ON TABLE upload_batch IS
    '上傳連結。連結本身只存 BLAKE3，持有者是登入帳號或 MCP Token 其中之一。';
COMMENT ON COLUMN upload_batch.used_files IS
    '已占用的檔案名額。上傳開始時加一，上傳失敗時退回。';

CREATE TABLE upload_staging (
    id uuid NOT NULL,
    batch_id uuid NOT NULL,
    file_name text NOT NULL,
    byte_size bigint NOT NULL,
    content_type text NOT NULL,
    checksum text NOT NULL,
    object_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    consumed_at timestamp with time zone,
    consumed_doc_no text,
    CONSTRAINT upload_staging_pkey PRIMARY KEY (id),
    CONSTRAINT upload_staging_object_key_key UNIQUE (object_key),
    CONSTRAINT upload_staging_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES upload_batch(id) ON DELETE CASCADE,
    CONSTRAINT upload_staging_byte_size_check CHECK ((byte_size > 0))
);

CREATE INDEX upload_staging_batch_id_idx ON upload_staging USING btree (batch_id);

COMMENT ON TABLE upload_staging IS
    '經上傳連結收到、尚未存成文件的檔案。存成文件後記下文件編號；一天內沒有取用就連同物件一起刪除。';

INSERT INTO permission (code, label, note, op_names, sort_order) VALUES
    ('manage_documents', '管理文件', '查看、上傳與驗證文件版本與內容雜湊', '{list_documents,upload_document,get_document,request_upload_link,list_uploads}', 85)
ON CONFLICT (code) DO UPDATE
    SET label = EXCLUDED.label, note = EXCLUDED.note,
        op_names = EXCLUDED.op_names, sort_order = EXCLUDED.sort_order;

INSERT INTO mcp_tool_grant (tool_name, enabled, risk, note, updated_at) VALUES
    ('request_upload_link', true, 'needs_confirm', '取得上傳連結，檔案經連結送進暫存區。', '2026-09-30 00:00:00+00'),
    ('list_uploads', true, 'read_only', '列出一條上傳連結已收到的檔案。', '2026-09-30 00:00:00+00')
ON CONFLICT (tool_name) DO UPDATE
    SET enabled = EXCLUDED.enabled, risk = EXCLUDED.risk,
        note = EXCLUDED.note, updated_at = EXCLUDED.updated_at;
