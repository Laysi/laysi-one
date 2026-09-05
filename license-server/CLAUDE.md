# CLAUDE.md

## 專案

Laysi One 授權伺服器：經銷商開立 Laysi One 的啟動金鑰，安裝端定期報到。
與產品在同一個 repo，但**是兩套系統、兩個資料庫、兩個 cargo workspace**——產品是
單租戶，這裡是多租戶，把它們合在一起等於要為經銷商在每一張產品資料表上加一個欄位。
共用的只有 `../backend/crates/laysi-license-token`：授權檔 payload 的唯一定義，
簽的一方與驗的一方都 `use` 它。

單一 crate，Rust + axum + sqlx + PostgreSQL 18。相依套件一律取自產品既有的依賴樹，
**不引入產品沒有的 crate**：這台機器持有簽章私鑰。**唯一的例外是 `lettre`**
（寄送核發的密碼），取 rustls 而非 OpenSSL，不帶 C 依賴。

工具鏈由 proto 管理，版本與 repo 根目錄的 `.prototools` 一致。`scripts/dev.ts` 把
`~/.cargo/bin` **接在 PATH 後面而不是前面**：那個目錄裡的 `cargo` 是 rustup 的 shim，
排在前面會讓建置落到 rustup 的預設工具鏈，兩個編譯器共用一個 `target` 會在對方編過的
第一個 crate 上以 E0514 失敗。

**映像的 build context 是 repo 根目錄**（`docker-compose.yml` 寫 `context: ..`），
因為那個共用 crate 在 `license-server/` 之外。改 `Dockerfile` 的 `COPY` 路徑時記得
它們是相對於根目錄，不是相對於本目錄。

## 部署

兩支 workflow，與產品同一套分工：

```bash
git tag license-v0.1.0 && git push origin license-v0.1.0   # 建映像推 ghcr
gh workflow run license-deploy.yml -f version=license-v0.1.0
```

**推標籤就部署**：`license-release.yml` 建完映像之後呼叫 `license-deploy.yml`
（`workflow_call`），所以上面第二個指令只在回退時才需要手動跑。傳的是標籤本身而不是
`latest`：主機要說得出自己跑哪一版。

**標籤前綴分開**：`v*` 是產品（`release.yml`），`license-v*` 是這裡
（`license-release.yml`）。兩個 pattern 不重疊，一個標籤只會觸發其中一支。少了前綴，
一次發版會同時建出兩邊的映像。

`license-deploy.yml` 從 ghcr 拉下指定標籤、`docker save | ssh podman load` 送到主機、
`podman-compose -f docker-compose.prod.yml up -d`。**指定標籤而不是從 main 建**，
所以回退就是再跑一次同一支 workflow 換一個版本。主機上沒有 registry 憑證。

**簽章私鑰由 `LICENSE_SIGNING_KEY` 這組 GitHub secret 提供，deploy 時寫進資料卷，
不進映像**：映像會被複製、快取、拉取，而這把金鑰輪替一次要跨越 75 天協調每一個安裝
升級（見產品的 `docs/architecture.md`），所以它的曝險面要壓到最小。寫入是
`[ -s ] ||`，資料卷裡已經有一把就不覆蓋——覆蓋等於現場每一張授權檔同時失效，那不是
部署該做的決定。

最後一步比對**執行中的伺服器公鑰與產品的 `PUBLIC_KEY_HEX`**，不同就讓部署失敗。
那個常數是手抄進產品原始碼的，沒有這一步，抄錯的症狀是客戶畫面上一句「授權檔無法驗證」。

第一次執行前要設好：repository variable `LICENSE_DEPLOY_HOST`，secrets
`LICENSE_SIGNING_KEY`（已設）與 `LICENSE_HOST_KEY`（主機的 SSH host key，格式如
`ssh-ed25519 AAAA…`）。

## 常用指令

```bash
bun scripts/dev.ts               # 開發用：podman 資料庫 + cargo watch（stop／reset）
cargo test                       # 不需要資料庫
cargo clippy --all-targets -- -D warnings
cargo fmt --check
docker compose up -d --build
docker compose exec app laysi-one-license create-vendor <email>   # 第一個後台帳號
docker compose exec app laysi-one-license public-key              # 編進產品的公鑰
```

## 慣例

- **程式註解與文件字串一律英文**，畫面文字一律繁體中文，商務語氣，與 laysi-one 同一套判準。
- **金鑰與授權檔是兩個東西**：啟動金鑰是客戶輸入的共享秘密（只存 BLAKE3 與前十碼），
  授權檔是本伺服器簽出去的 Ed25519 payload。`src/token.rs` 兩者都在。
- **schema.sql 全部 `IF NOT EXISTS`**，啟動時整份套用，沒有 migration 版本號。
  這台伺服器的資料表沒有第二個寫入者。
- 查詢一律執行期 `sqlx::query`，編譯不需要資料庫。動態拼接的 SQL 只拼
  `Actor::scope_sql()` 的 `i32`，其餘一律 bind。
- 不新增需要人自己想一個值的環境變數：資料庫密碼與簽章金鑰由 compose 的 `secrets`
  服務產生進資料卷，**產生過就不再產生**（換簽章金鑰等於作廢所有已發出的授權檔）。

## 四個設計決定

### 1. 一個端點

啟用與定期檢查是同一個 `POST /v1/license/check`。第一次呼叫綁定主機識別碼並把
`status` 由 `issued` 推到 `active`，之後每一次都是續期。沒有第二個端點，因為沒有
第二個決定。

### 2. 租期是唯一的日期

回應裡的 `not_after = min(今天 + LEASE_DAYS, expires_on)`，安裝端整套狀態機只看它。
`expires_on` 只給人看。合約已經到期時**仍然發出授權檔**（`not_after` 是過去），
安裝端才說得出到期日與該找誰；403 帶不了這兩件事。

### 3. 綁定拒絕，重複安裝只記錄

指紋不同一律 403 並寫 `license_denial`——見到就重綁等於沒有綁。但**連續性 nonce
不對只計數不擋**：從昨天的備份還原與整份複製長得一模一樣，用啟發式把客戶鎖在自己的
帳本外面是更嚴重的錯。兩者都出現在後台的授權清單上。

### 4. 不簽離線授權檔

曾經有過：後台貼進主機識別碼就簽一份 `not_after = expires_on` 的授權檔，不需回報。
拿掉的理由不是簽章弱（與線上那條路同一把私鑰、同樣綁指紋），是**發出去就撤銷不了**，
而且那台主機在這裡完全看不見。線上安裝停止發放約 75 天後自動轉唯讀，離線的那份要等到
合約到期日。產品端一併移除，`POST /licenses/{no}/offline` 不存在。

### 5. 經銷商是租戶，成員自己管

`partner` 一家一列，**沒有階層**：代理商與經銷商曾經是同一張表上的兩層
（`parent_id`），拿掉的理由是兩者在這裡做的事完全相同，而那一層讓
`scope_sql` 多一個 OR、建立表單多一個下拉、每一頁多一個欄位。

`partner_user` 是成員。`is_vendor` 是原廠員工，`is_admin` 是該租戶的管理員，
兩者皆無就是一般成員。**授權範圍只有一個判斷**：`Actor::may_manage(partner_id)`
——原廠管每一家，管理員只管自己那家，一般成員誰也管不了，包含自己。

新成員的 `is_vendor` **由所在租戶推導**（該租戶已有原廠帳號就是原廠員工），
不是表單欄位，所以 `P-000` 這個代號只寫在 `create-vendor` 一處。

兩道拒絕擋住把自己鎖在外面：最後一位使用中的管理員不得停用或降級（清點時
`is_admin OR is_vendor`，與 `may_manage` 同一個定義），以及任何人不得變更本帳號的
權限與狀態——能撤銷那個變更的帳號就是正在被改的那一個。

### 6. 授權費只有原廠設定

`partner.unit_price` 由原廠在「經銷商」頁設定，開立與續約時複製到 `license_term`。
經銷商的畫面上沒有金額：開立與續約的表單沒有授權費欄位，授權詳情的期間表格不畫那一欄，
「月結對帳」整頁只有原廠進得去。改價不影響已經賣出的授權，因為金額是賣出當下複製的。

### 7. 核發的密碼寄出去，首次登入必須換掉

`must_change_password` 由三條路設起：`create-vendor`、建立成員、管理員重設密碼。
設起之後 `signed_in!` 把每一個畫面導向 `/password`，換完才放行。本人變更密碼不撤銷
工作階段（剛證明持有舊密碼），管理員重設則撤銷全部。

密碼由 `SMTP_URL` 與 `SMTP_FROM`（兩者都吃 `_FILE`）寄給本人。**這兩個是環境變數而
不是畫面設定**：這台伺服器是原廠自己的單一部署，不是客戶安裝的東西，沒有第二個人要填它。
兩者缺一或寄送失敗都不算錯誤——畫面退回原本的「密碼只顯示一次」，因為密碼終究要有人拿到。
