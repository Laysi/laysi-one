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

`license-deploy.yml` 送 compose 檔與寫著 `LICENSE_VERSION` 的 `.env`、讓**主機自己**
從 ghcr 拉下指定標籤、`podman-compose -f docker-compose.prod.yml up -d`。
**指定標籤而不是從 main 建**，所以回退就是再跑一次同一支 workflow 換一個版本。

**主機拉，而不是 runner 拉完再串過 tunnel**：registry 會給每台機器對得上的那一份，所以
這支 workflow 沒有平台要猜——產品那邊猜錯的那次載進了主機執行不了的映像，站台 502
二十五分鐘（`deploy.yml` 因此先改）。憑證是那次 run 現鑄的 `GITHUB_TOKEN`，由 workflow
的 `permissions` 限成 `packages: read`、job 結束即失效，用完 `trap` 登出，所以
**主機上不留 registry 憑證**。

`docker-compose.prod.yml` 的 `app.image` 因此是 registry 位址加 `${LICENSE_VERSION}`，
不是本機標籤——主機拉不了一個不存在於任何 registry 的名字，而且這樣主機說得出自己跑哪
一版。

**簽章私鑰由 `LICENSE_SIGNING_KEY` 這組 GitHub secret 提供，deploy 時寫進資料卷，
不進映像**：映像會被複製、快取、拉取，而這把金鑰輪替一次要跨越 75 天協調每一個安裝
升級（見產品的 `docs/architecture.md`），所以它的曝險面要壓到最小。寫入是
`[ -s ] ||`，資料卷裡已經有一把就不覆蓋——覆蓋等於現場每一張授權檔同時失效，那不是
部署該做的決定。

最後一步比對**執行中的伺服器公鑰與產品的 `PUBLIC_KEY_HEX`**，不同就讓部署失敗。
那個常數是手抄進產品原始碼的，沒有這一步，抄錯的症狀是客戶畫面上一句「授權檔無法驗證」。

第一次執行前要設好：repository variable `LICENSE_DEPLOY_HOST` 與 secret
`LICENSE_SIGNING_KEY`（已設）。主機的 SSH host key 寫在 workflow 檔案裡，與
`deploy.yml` 同一種寫法——公鑰不是機密，而藏在讀不到的 secret 裡，它指著一台已經
退役的主機三天都沒有人發現。

**映像建 amd64 與 arm64 兩種**：build stage 留在 runner 自己的架構上交叉編譯，
只有 runtime stage 走模擬。原本只建 amd64，主機換成 ARM 板子之後映像就跑不起來。

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
  **金鑰重新產生得了，但只限尚未啟用的授權**（`fingerprint IS NULL`）：客戶在啟用前
  弄丟金鑰，唯一的退路本來是重開一張授權，而月結對帳數的是 `license_term`，等於自己
  多開一次帳單。已綁定的不給——每一次回報都以 `key_hash` 找那一列，換掉等於租期用完
  就轉唯讀。
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
沒有 `is_admin` 就是一般成員，原廠員工也一樣。**授權範圍只有一個判斷**：
`Actor::may_manage(partner_id)`——原廠的管理員管每一家，經銷商的管理員只管自己那家，
一般成員誰也管不了，包含自己。**`is_vendor` 放寬的是看得到什麼，不是管得到誰**：
原本 `may_manage` 對 `is_vendor` 一律放行，而原廠租戶底下每一個成員都是 `is_vendor`，
於是原廠的一般成員升降得了任何人的管理員身分、重設得了原廠管理員的密碼（沒設寄件時
密碼直接印在他的畫面上）。

新成員的 `is_vendor` **由所在租戶推導**（該租戶已有原廠帳號就是原廠員工），
不是表單欄位，所以 `P-000` 這個代號只寫在 `create-vendor` 一處。

兩道拒絕擋住把自己鎖在外面：最後一位使用中的管理員不得停用或降級（清點時
只數 `is_admin`，與 `may_manage` 同一個定義），以及任何人不得變更本帳號的
權限與狀態——能撤銷那個變更的帳號就是正在被改的那一個。

### 6. 授權費只有原廠設定，期間以年為單位

`partner.unit_price` 是**一個授權年**的價格，由原廠在「經銷商」頁設定，開立與續約時
乘上年數寫進 `license_term.amount`。經銷商的畫面上沒有金額：開立與續約的表單沒有授權費
欄位，授權詳情的期間表格不畫那一欄，「月結對帳」整頁只有原廠進得去。改價不影響已經賣出
的授權，因為金額是賣出當下算好的。

**經銷商選年數，不選日期**（開立與續約各一個 `1`～`MAX_TERM_YEARS` 的下拉）。一次賣
幾年由經銷商決定，一次付清；他之後怎麼跟客戶收是他們兩方的事。到期日由伺服器推算——
開立從今天起算，續約從原到期日（已過期就是今天）起算，`console.rs` 的 `term_end` 與
續約 SQL 的 `make_interval(years => …)` 各算一次，兩邊都以曆月推算，所以 2/29 開立的
三年期落在 2031-02-28。原本兩張表單各有一個自由的日期欄，而 `amount` 一律記一年的單價
——賣三年與賣半年在月結對帳上是同一個數字，等於付錢的一方自己決定要付幾年的錢。
**年數超出範圍一律 400，不夾到邊界**：那個數字同時決定期間與金額，靜靜賣成另一個
比退回表單更糟。

### 6.1 月結對帳一次只看一個月

「月結對帳」是原廠向經銷商請款的依據，所以版面就是請款單的版面：選一個月份，**一家
經銷商一段**，段首是名稱與統一編號、段尾是小計——那個小計就是這個月要跟他請的金額，
不必再從別處加一次。段內逐筆列出授權編號、被授權單位、種類（開立／續約）、授權期間
與金額，經銷商問「這個數字怎麼來的」時答案在同一頁上。

**明細與小計來自同一次讀取**：`db::billing` 回逐筆的列，畫面用 `chunk_by` 依經銷商
分段再相加，不另外下一句 `GROUP BY`——兩句各自聚合就有兩個答案對不上的可能。
**開立與續約由列的順序推導**（該授權最小的 `license_term.id`），不存欄位。
月份下拉只列真的有紀錄的月份，預設最新的一個而不是當月：月初開這一頁是為了結算剛
結束的那個月。統一編號沒填的段落標記出來，那是開發票當下才發現就太晚的東西。
**金額未含營業稅**，`unit_price` 是未稅單價，稅由原廠的帳務系統加。

### 6.2 月結報告推送給原廠自己

網址填在「月結對帳」頁自己一張卡（`vendor_setting` 單列表，`statement_webhook_url`），
**不是環境變數**：填它的就是讀報告的人，改一個網址不該動到部署。三顆鈕儲存／測試推送／
清除，測試送的是**畫面上那個網址**而不是存起來的，所以網址存進去之前試得出來。

**每月一日推送上個月**，`statement::watch` 每小時看一次日曆。**沒有「產出」這個動作**
——月結束的那一刻報告就存在了，這裡只是說出來。**先認領月份再送、送失敗才把月份還
回去**（`claim_statement_month`／`release_statement_month`）：認領是那句 UPDATE 本身，
所以兩個行程對同一個資料庫只有一個搶得到；還回去讓下一次 tick 重試，成功的那一次不會
送第二遍。網址存檔時把 `notified_month` 記成**上個月**，所以九月中設定的網址是十月一日
送九月，而不是把之前每一個月補送一輪。**沒有紀錄的月份認領後不送**：沒有帳單可請款，
不送與送一份零元一樣清楚。

**payload 帶 `text` 與結構化欄位**：原廠的帳務系統要的是逐家經銷商的金額，聊天室讀的是
那一行字。**Lark／飛書是唯一的例外**：它讀 `msg_type` 底下的 `content.text`，
`statement::body_for` 對那兩個主機換成這個形狀，其餘主機照送整份。**Lark 對送錯形狀的
訊息一樣回 200**，拒絕寫在 body 的 `code`，所以 `deliver` 對 Lark 另讀 body：只看狀態碼
的版本在測試鈕上印「已推送」，排程那一側則把月份認領掉、永遠不再送。
**開了簽名校驗的 Lark 機器人**要在同一張卡填密鑰（`statement_webhook_secret`），每則訊息
帶 `timestamp` 與 `sign`（`timestamp` 換行接密鑰當 HMAC-SHA256 的鍵、簽空字串、base64），
時間戳在送出當下取，Lark 拒收超過一小時的簽名。密鑰只對 Lark 網址有意義，填在其他網址
旁邊存不進去。

### 7. 核發的密碼寄出去，首次登入必須換掉

`must_change_password` 由三條路設起：`create-vendor`、建立成員、管理員重設密碼。
設起之後 `signed_in!` 把每一個畫面導向 `/password`，換完才放行。本人變更密碼不撤銷
工作階段（剛證明持有舊密碼），管理員重設則撤銷全部。

密碼由 `SMTP_URL` 與 `SMTP_FROM`（兩者都吃 `_FILE`）寄給本人。**這兩個是環境變數而
不是畫面設定**：這台伺服器是原廠自己的單一部署，不是客戶安裝的東西，沒有第二個人要填它。
兩者缺一或寄送失敗都不算錯誤——畫面退回原本的「密碼只顯示一次」，因為密碼終究要有人拿到。

寄件走 Amazon SES（東京 `ap-northeast-1`，網域識別 `laysi.one`，Easy DKIM）。
**台北 `ap-east-2` 沒有 SES**，該區的服務清單裡沒有這一項，端點也不存在；寄一封核發
密碼的信，東京與台北的延遲差別看不出來。寄件地址是 `no-reply@laysi.one`，寫在
`docker-compose.prod.yml` 裡——它是這個部署決定好的常數，不是要人自己想一個值。

`SMTP_URL` 是 GitHub secret，`license-deploy.yml` 每次部署都覆寫進資料卷。**與簽章
金鑰相反**：輪替一組 SES SMTP 帳密是例行的事，現場沒有任何東西依賴它不變；secret
沒設就寫進一個空檔案，等於關掉寄件而不是讓部署失敗。

`laysi.one` 的 DMARC 是 `p=reject; adkim=s; aspf=s`。SES 預設的信封寄件網域是
`amazonses.com`，對不齊 SPF，**所以 DMARC 靠 DKIM 對齊過關**：識別建在網域本身而不是
子網域，`d=` 才會剛好是 `laysi.one`，嚴格對齊才成立。改成子網域識別或關掉 DKIM，
信會被收件方直接退回。
