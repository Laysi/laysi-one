# Laysi One

面向台灣中小企業的財務＋人資系統。Rust（axum + sqlx）＋ React（Vite + TS）＋
PostgreSQL 18 ＋ RustFS，以 Docker Compose 自架。

**授權：Business Source License 1.1** · **狀態：0.1.0，開發中**

新安裝只有基礎科目、權限、流程與法規設定，公司資料登入後自行建立。

跨層契約與設計取捨見 [docs/architecture.md](docs/architecture.md)。

## 快速開始

需要 Podman 或 Docker，以及對應的 Compose。

```bash
git clone <repo> && cd laysi_one
docker compose up -d --build
```

**不需要 `.env`，也沒有任何值要自己想。** `secrets` 服務在第一次啟動時產生資料庫密碼
與初始化代碼，寫進一個共用資料卷，其餘服務讀檔取用；已經產生過就不再產生，所以重跑
不會把既有資料庫的密碼換掉。

前端在 http://localhost:8088。第一次開啟會出現「建立系統管理員帳號」畫面，
輸入 `setup/初始化代碼.txt` 裡的代碼，建立帳號並自訂密碼後進入公司資料設定；該畫面
在第一個帳號建立後關閉。

代碼只在第一個帳號建立前有用，之後那個檔案可以刪除；刪掉就不會再產生（真的要找回來
是 `docker compose logs secrets`）。

第一個帳號是**系統管理員**：管帳號、權限與系統設定，看不到薪資，不能簽核或設預算。
負責人與其他成員的帳號由系統管理員在「角色與核准」建立。

不用網頁初始化就設 `SETUP_ALLOW_WEB=false`，改用指令：

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
backend/regulation/ 法規規則檔出廠預設值（依年度分目錄）
backend/regulation-updates/ 隨版本送出的法規新版，與 migrations 各記各的
backend/migrations/ SQL migration，啟動時自動套用
backup/             異地備份容器：pg_dump、age、S3
setup/              首次啟動寫入的初始化代碼，建立帳號後可刪
web/                Vite + React + TypeScript 前端
docs/architecture.md 跨層契約，動型別或端點前先讀
```

## 開發

工具鏈由 proto 管理，版本釘在倉庫根目錄的 `.prototools`：

```bash
proto install              # rust、zig、node
cargo install cargo-watch  # scripts/dev 用它自動重建重啟伺服器

# 資料庫與物件儲存在容器裡，伺服器與前端跑在本機
scripts/dev                # Ctrl-C 只停腳本起的那些，容器留著
scripts/dev stop           # 連容器一起停

cd backend

cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# 改過 laysi-ops 後重新產生前端型別
cargo run -p laysi-server --bin laysi-schema -- ../web/src/api/generated.ts

# 改過品牌檔後重簽（否則伺服器拒絕啟動）
cargo run -p laysi-brand --bin laysi-brandhash -- --update
```

本機沒有系統 C 編譯器時（`ring` 之類的相依要編 C 原始碼），把 `scripts/toolchain`
放進 PATH，裡面的 `cc` 與 `ar` 會轉呼叫 proto 裝的 zig：

```fish
set -x PATH (pwd)/scripts/toolchain $PATH
```

管理指令直接在跑著的 api 容器裡執行：

```bash
docker compose exec api laysi-admin list-users
```

前端：

```bash
cd web && npm run dev      # 5173，proxy /v1 與 /mcp 到 localhost:8080
npm run build              # tsc --noEmit && vite build
```

編譯不需要資料庫連線。

## 接上 AI 用戶端（MCP）

以 **Bearer token** 連線，適用 Claude Code、Claude 桌面版這類直接連 MCP 的用戶端。

1. 於「MCP 與 AI」畫面呼叫 `issue_api_token`，`kind` 填 `mcp`。回應的 `secret`
   只顯示一次，請當下複製保存。
2. `docker compose up -d` 後，`mcp` 服務發布 `${MCP_PORT:-8089}:8080`。本機測試可
   直接用 `http://localhost:8089/mcp`。
3. 要給其他機器連，請自行架反向代理處理 TLS 與網域，把 `你的網域/mcp` 代理到
   `MCP_PORT`，記得放行 `POST` 與長連線。
4. 加進用戶端：

   ```bash
   claude mcp add --transport http https://你的網域/mcp \
     --header "Authorization: Bearer lys_...."
   ```

claude.ai 網頁版還連不進來：它要的是 OAuth 2.1。

## 異地備份

備份的目的地在畫面上：「安全與營運」填相容 S3 介面的儲存位置、桶子與存取金鑰，`backup`
服務每天把加密後的資料庫備份與文件副本送過去。設定改了不必重新啟動。

第一次啟用時系統產生一組還原金鑰，**私鑰只顯示一次**，按下「已保存」之後系統不再留存，
備份才開始執行——確認之前不備份，是為了讓沒有任何一份備份含有開啟它自己的鑰匙。
金鑰遺失就無法還原，請保存在系統之外。

還原：

```bash
age -d -i <保存的還原金鑰檔> laysi-<時間>.dump.age | pg_restore -d <目標資料庫>
```

## 升級須知

### 0.1.0 開發資料庫

本版本改寫了既有的 `0002`、`0004`、`0005`、`0006`、`0007` 與 `0008` migration。
migration checksum 會驗證，套用過舊版的開發資料庫起不來，升級前請重建：

```bash
docker compose down -v
docker compose up -d --build
```

`down -v` 會刪除 Compose 管理的資料卷，需要保留的開發資料請先自行備份。

## 授權

Business Source License 1.1，見 [LICENSE-BSL.txt](LICENSE-BSL.txt)。原始碼公開
可讀、可自架自用，但不得提供為競爭性的商業服務；Change Date 之後轉為 Apache 2.0。

品牌字標由 `laysi-brand` 產生並簽章驗證，改過品牌檔後要重簽（見「開發」）。
