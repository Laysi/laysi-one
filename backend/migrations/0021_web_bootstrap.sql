-- Laysi One: record that this installation claimed its first account, so the
-- web bootstrap can happen exactly once.
--
-- Creating the first account is the one request no permission check can
-- guard: there is nobody to be signed in as yet. The gate is therefore the
-- state of the installation itself, and the application is not the last line
-- of defence for it any more than it is for the ledger invariants.
--
-- `install_bootstrap` holds at most one row. The primary key with
-- `CHECK (id = 1)` means a second bootstrap raises a unique violation inside
-- the same transaction that would create the account. Two concurrent requests
-- therefore cannot both succeed, and deleting every `user_account` row
-- afterwards does not reopen the window.
--
-- Accounts created by `laysi-admin` leave no row here; the CLI runs on the
-- host and needs no such gate. An installation bootstrapped from the CLI has
-- an empty table, and the web endpoint stays closed because `user_account` is
-- not empty.

CREATE TABLE install_bootstrap (
    id             int PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    at             timestamptz NOT NULL DEFAULT now(),
    -- The login name that was created, kept as a trace of who claimed this
    -- installation. Not a foreign key: the account may later be renamed or
    -- removed, and this row records what happened at the time.
    username       text NOT NULL,
    user_agent     text NOT NULL DEFAULT '',
    -- Whether SETUP_TOKEN was in force for this bootstrap. An installation
    -- claimed without one is a different security story from one that needed
    -- a shared secret, and only this row can say which it was.
    token_required boolean NOT NULL
);

COMMENT ON TABLE install_bootstrap IS
    '網頁初始化只能成功一次；單列主鍵是資料庫層的保證，不只靠應用層檢查。';
COMMENT ON COLUMN install_bootstrap.token_required IS
    '記錄當時是否要求 SETUP_TOKEN；沒有這一欄就無法事後判斷這台是怎麼被認領的。';
