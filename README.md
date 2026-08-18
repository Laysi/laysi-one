# Laysi One

面向台灣中小企業的財務＋人資系統。Rust（axum + sqlx）＋ React（Vite + TS）＋
PostgreSQL 17 ＋ RustFS，以 Docker Compose 自架。

**授權：Business Source License 1.1** · **狀態：0.1.0，開發中**

新安裝只建立系統運作所需的基礎科目、權限、流程與法規設定，不預載任何公司的
營運或人事資料。

## 特色

- **所有操作都支援 MCP**：REST、MCP 工具、TypeScript 型別由同一份操作註冊表
  產生，不會出現「畫面做得到、AI 做不到」的落差。
- **法規數值不寫在程式裡**：費率、級距、上限、天數放在 `backend/regulation/`
  的 TOML，依日期解析。修法只要新增一份生效日較晚的規則檔。
- **帳務不變量由資料庫強制**：借貸相等、已過帳傳票不可修改，是 CHECK 與
  trigger，不是只靠應用層。
- **報表由總帳現算**：系統裡沒有一張存放報表金額的資料表。
- **權限 deny-by-default**：每次呼叫即時查角色權限，被擋下的呼叫一樣寫稽核。

## 快速開始

需要 Docker 與 Docker Compose。

```bash
git clone <repo> && cd laysi_one
cp .env.example .env        # 先改掉密碼與金鑰
docker compose up -d --build
```

前端在 http://localhost:8088。系統不提供預設帳號，第一次開啟時會出現「建立系統管理員
帳號」畫面，在上面建立第一個帳號並自訂密碼，接著直接進入公司資料設定。這個畫面只在
資料庫完全沒有帳號時出現，成功一次之後永久關閉。

第一個帳號是**系統管理員**，不是負責人：管帳號、權限與系統設定，但看不到薪資、
不能簽核、不能設預算。負責人與其他成員的帳號由系統管理員在「角色與核准」建立。

初始化是未登入即可呼叫的端點，所以先有先得。部署在公開位址時，請在 `.env` 設
`SETUP_TOKEN` 為一段長亂數，初始化畫面會要求輸入；不想用網頁初始化就設
`SETUP_ALLOW_WEB=false`，改用指令：

```bash
docker compose exec api laysi-admin create-user \
  --username admin --role admin --display-name 系統管理員
```

指令會把一次性的暫時密碼印在 stdout，首次登入必須更換。角色代碼有
`admin`、`boss`、`acct`、`cashier`、`manager`、`accountant`、`staff`。

## 專案結構

```
backend/crates/
  laysi-brand       品牌字標產生與完整性驗證
  laysi-domain      純領域：Money、分錄、借貸平衡
  laysi-regulation  法規引擎：TOML 規則載入、生效日解析
  laysi-store       sqlx repository、RustFS 存取
  laysi-ops         Operation trait + 註冊表 + 各業務模組
  laysi-server      axum：/v1 REST、/mcp、/healthz
backend/regulation/ 法規規則檔（依年度分目錄）
backend/migrations/ SQL migration，啟動時自動套用
web/                Vite + React + TypeScript 前端
docs/architecture.md 跨層契約，動型別或端點前先讀
```

## 開發

工具鏈由 proto 管理，版本釘在倉庫根目錄的 `.prototools`，不需要 `sqlx-cli`：

```bash
proto install              # rust、zig、node
cd backend

cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# 改過 laysi-ops 後重新產生前端型別
cargo run -p laysi-server --bin laysi-schema -- ../web/src/api/generated.ts

# 改過品牌檔後重簽（否則伺服器拒絕啟動）
cargo run -p laysi-brand --bin laysi-brandhash -- --update
```

Rust 在 Linux 透過 `cc` 連結，`ring` 之類的相依還會編 C 原始碼。本機沒有系統 C
編譯器時，把 `scripts/toolchain` 放進 PATH，裡面的 `cc` 與 `ar` 會轉呼叫 proto
裝的 zig，不必另外裝 gcc：

```fish
set -x PATH (pwd)/scripts/toolchain $PATH
```

那個目錄不會進映像——Dockerfile 只複製 `LICENSE-BSL.txt` 與 `backend/`，容器建置
仍用 rust 映像自己的工具鏈。

管理指令直接在跑著的 api 容器裡執行：

```bash
docker compose exec api laysi-admin list-users
```

前端：

```bash
cd web && npm run dev      # 5173，proxy /v1 與 /mcp 到 localhost:8080
npm run build              # tsc --noEmit && vite build
```

查詢一律用執行期的 `sqlx::query`（不是 `query!` 巨集），所以編譯不需要資料庫連線。

## 接上 AI 用戶端（MCP）

本階段支援 **Bearer token**，適用直接連 MCP 的用戶端（Claude Code、Claude 桌面版）。

1. 於「MCP 與 AI」畫面呼叫 `issue_api_token`，`kind` 填 `mcp`。回應的 `secret`
   只顯示一次，系統無法回查，請當下複製保存。
2. `docker compose up -d` 後，`mcp` 服務會發布 `${MCP_PORT:-8089}:8080`，是整套
   compose 唯一對外的後端埠；`api` 不發布任何埠。本機測試可直接用
   `http://localhost:8089/mcp`。
3. 要給其他機器連，請自行架反向代理處理 TLS 與網域（專案不內建 Caddy／Traefik），
   把 `你的網域/mcp` 代理到 `MCP_PORT`，記得放行 `POST` 與長連線。
4. 加進用戶端：

   ```bash
   claude mcp add --transport http https://你的網域/mcp \
     --header "Authorization: Bearer lys_...."
   ```

**claude.ai 網頁版目前接不進來**：它要公開的 HTTPS 網址，而且用的是 OAuth 2.1
而非 Bearer token。OAuth 授權流程排在下一階段。

## 升級須知

### 0.1.0 開發資料庫

本版本直接修改既有的 `0002`、`0004`、`0005`、`0006`、`0007` 與 `0008`
migration，以移除先前預載的示範營運資料。`sqlx::migrate!()` 會驗證 migration
checksum，套用過舊版 migration 的開發資料庫會拒絕啟動。

0.1.0 尚未正式部署，不提供資料轉換 migration。升級前請重建開發資料庫：

```bash
docker compose down -v
docker compose up -d --build
```

`down -v` 會刪除 Compose 管理的資料卷，需要保留的開發資料請先自行備份。

## 授權

Business Source License 1.1，見 [LICENSE-BSL.txt](LICENSE-BSL.txt)。**這不是
OSI 認可的開源授權**：原始碼公開可讀、可自架自用，但不得提供為競爭性的商業
服務；授權條款所訂的 Change Date 之後轉為 Apache 2.0。

品牌字標由 `laysi-brand` 以參數化幾何即時產生，並以 BLAKE3 與 Ed25519 簽章在
啟動時驗證；驗證失敗伺服器會拒絕啟動。
