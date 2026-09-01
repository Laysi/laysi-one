-- OAuth 2.1 authorization server, so a remote AI connector can obtain an MCP
-- token by sending its user here to sign in instead of being handed a secret
-- to paste.
--
-- 0001 is applied and frozen, so everything here is additive.
--
-- There is no access token table: the token endpoint mints an ordinary
-- `api_token` row with kind = 'mcp'. That is what the MCP endpoint already
-- authenticates against, and it is what 「安全與營運」 already lists and
-- revokes. A second token store would be a second place to revoke.

-- One row per connector that registered itself (RFC 7591). The clients are
-- public: they hold no secret and prove possession of the request with PKCE
-- instead, which is what a client running in someone else's cloud can
-- actually keep.
CREATE TABLE oauth_client (
    client_id text NOT NULL,
    client_name text DEFAULT ''::text NOT NULL,
    redirect_uris text[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT oauth_client_pkey PRIMARY KEY (client_id),
    CONSTRAINT oauth_client_redirect_uris_check CHECK (cardinality(redirect_uris) > 0)
);
COMMENT ON TABLE oauth_client IS
    '自行註冊的外部連接器。沒有密鑰，改以 PKCE 證明持有請求。';
COMMENT ON COLUMN oauth_client.redirect_uris IS
    '註冊當下固定。授權碼只會送到這份清單裡逐字相符的網址。';

-- A single-use authorization code, alive for minutes. The plaintext exists
-- only in the redirect; the table keeps a BLAKE3 hash, the same shape as
-- `api_token` and `user_session`.
CREATE TABLE oauth_code (
    code_hash text NOT NULL,
    client_id text NOT NULL,
    user_id integer NOT NULL,
    redirect_uri text NOT NULL,
    code_challenge text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    CONSTRAINT oauth_code_pkey PRIMARY KEY (code_hash),
    CONSTRAINT oauth_code_client_id_fkey FOREIGN KEY (client_id)
        REFERENCES oauth_client(client_id) ON DELETE CASCADE,
    CONSTRAINT oauth_code_user_id_fkey FOREIGN KEY (user_id)
        REFERENCES user_account(id) ON DELETE CASCADE
);
CREATE INDEX oauth_code_expiry_idx ON public.oauth_code USING btree (expires_at);
COMMENT ON COLUMN oauth_code.code_challenge IS
    'PKCE S256 challenge。換發時比對 code_verifier，plain 一律不收。';
COMMENT ON COLUMN oauth_code.redirect_uri IS
    '核發當下用的那一個。換發時要一字不差，否則授權碼可以被導到別的網址。';
