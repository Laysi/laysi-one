-- Once external sign-in is verified working, a company may not want a
-- password to be a way in at all. `password_login_disabled` keeps one
-- account role able to still use a password: the system administrator,
-- because turning this on and then having the IdP break would otherwise
-- strand the whole installation with no way to sign in and no way to reach
-- the settings screen that could turn it back off.
--
-- The flag has no effect by itself: `oidc::current()` only builds a client
-- when discovery and the stored settings actually work, and the login
-- handler only enforces this flag against the client that call returns. A
-- broken IdP makes `current()` return nothing, which makes password login
-- available to everyone again without anybody having to notice and flip the
-- switch back.

ALTER TABLE oidc_setting
    ADD COLUMN password_login_disabled boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN oidc_setting.password_login_disabled IS
    '啟用時非系統管理員帳號只能用外部登入；系統管理員例外，避免外部登入設定壞掉時整個系統進不去。';
