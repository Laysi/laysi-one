# 授權伺服器的異地備份

授權資料庫是整條銷售鏈的唯一一份紀錄：每一家經銷商、每一組已售出的啟動金鑰，
以及月結對帳所依據的 `license_term`。主機掉了就沒有第二份。

每天 04:30（台北）由 systemd timer 執行 `backup.sh`，`pg_dump` 之後以 age 加密，
送進 Cloudflare R2 的 `laysi-one-license` bucket。

## 三個性質

- **主機上只有加密能力。** age 金鑰對在本機產生，公鑰放主機、私鑰進密碼管理程式。
  主機被拿下的人拿得到備份，開不了備份。
- **簽章私鑰不進 R2。** 它已經在 `LICENSE_SIGNING_KEY` 這個 repository secret 與本機
  `license-server/secrets/`。第三份放進備份，等於讓 age 私鑰變成單點：弄丟它，現場
  每一份授權檔一起作廢。
- **備份刪不掉。** `db/` 前綴有 90 天的 R2 bucket lock，主機上那組 token 外洩也刪不掉、
  蓋不掉既有備份；180 天後由 lifecycle 規則自動清除。

## 首次設定

### 1. R2 bucket 與保留規則

已經建好（`laysi-one-license`，APAC，`db/` 前綴 90 天 lock ＋ 180 天 lifecycle）。
重建時：

```bash
wrangler r2 bucket create laysi-one-license --location apac
wrangler r2 bucket lock add laysi-one-license --name db-90-days --prefix db/ --retention-days 90
```

### 2. R2 API token

在 Cloudflare 面板 **R2 → API → Manage API tokens → Create API token**：

- Permissions：**Object Read & Write**
- Specify bucket：只勾 `laysi-one-license`
- TTL：Forever

記下 Access Key ID、Secret Access Key 與 Account ID。**不要用 Account API token**：
那把鑰匙開得了整個帳號，而這裡只需要一個 bucket。

### 3. age 金鑰對

在本機產生，不在主機上：

```bash
podman run --rm --entrypoint age-keygen ghcr.io/laysi/laysi-one-backup:latest 2>/dev/null
```

`--entrypoint` 不可省：該映像的進入點是產品的備份迴圈，省掉之後 `age-keygen` 會被當成
那個迴圈的引數。

輸出的 `AGE-SECRET-KEY-…` 進密碼管理程式，`age1…`（公鑰）填進下一步。

### 4. 主機上的設定檔

```bash
ssh <host> 'cat > ~/.config/laysi-license-backup.env <<EOF
R2_ACCOUNT_ID=…
R2_ACCESS_KEY_ID=…
R2_SECRET_ACCESS_KEY=…
AGE_RECIPIENT=age1…
EOF
chmod 0600 ~/.config/laysi-license-backup.env'
```

### 5. 安裝 timer

`.github/workflows/license-deploy.yml` 每次部署會送檔並啟用 timer。手動安裝：

```bash
scp backup.sh <host>:~/laysi-one-license/
scp laysi-license-backup.service laysi-license-backup.timer <host>:~/.config/systemd/user/
ssh <host> 'export XDG_RUNTIME_DIR=/run/user/$(id -u)
  # 沒有 linger，登出時 user manager 跟著停，04:30 那一次不會發生。
  loginctl enable-linger $(id -un)
  systemctl --user daemon-reload
  systemctl --user enable --now laysi-license-backup.timer
  systemctl --user start laysi-license-backup.service
  systemctl --user status laysi-license-backup.service --no-pager -l'
```

設定檔缺任何一項時，服務會以非零退出並指名缺的是哪一個。

**看輸出用 `systemctl --user status`，不是 `journalctl --user`**：主機上的 `ai-km`
不在 `systemd-journal` 群組，`journalctl --user` 一律回「No journal files were found」，
而單元自己的 status 仍然帶著最後幾行。

## 還原

每季演練一次。沒有演練過的備份不算備份。

在自己的機器上做，不在主機上——私鑰不進主機。先把設定值放進這個 shell（值與主機
`~/.config/laysi-license-backup.env` 相同）：

```bash
export R2_ACCOUNT_ID=…
export AWS_ACCESS_KEY_ID=…          # R2_ACCESS_KEY_ID
export AWS_SECRET_ACCESS_KEY=…      # R2_SECRET_ACCESS_KEY
export AWS_DEFAULT_REGION=auto
```

```bash
# 1. 取最新一份
aws --endpoint-url "https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com" \
    s3 ls s3://laysi-one-license/db/ | tail -1
aws --endpoint-url "https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com" \
    s3 cp s3://laysi-one-license/db/license-<stamp>.dump.age .

# 2. 解密（私鑰從密碼管理程式取出，存成 key.txt）
age -d -i key.txt license-<stamp>.dump.age > license.dump

# 3. 還原到拋棄式資料庫
podman run -d --name restore-check -e POSTGRES_PASSWORD=x -p 5434:5432 postgres:18
# `podman run -d` 回來時資料庫還沒接受連線，少了這一圈就是 connection refused。
until podman exec restore-check pg_isready -U postgres >/dev/null 2>&1; do sleep 2; done
pg_restore -d postgres://postgres:x@localhost:5434/postgres --no-owner license.dump
psql postgres://postgres:x@localhost:5434/postgres -c 'SELECT count(*) FROM license'
podman rm -f restore-check
```

比對第三步的筆數與線上相同，演練就算通過。

**簽章私鑰另外驗**：它不在備份裡，所以要確認密碼管理程式那一份仍然是對的——
`laysi-one-license public-key` 的輸出要等於 `backend/crates/laysi-ops/src/ops/license.rs`
的 `PUBLIC_KEY_HEX`。兩者不同，現場每一個安裝都會拒絕新簽出來的授權檔。
