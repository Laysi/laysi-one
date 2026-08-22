# MCP 測試清單

這一份驗介面：協定、認證、授權與稽核。算出來的數字對不對在
[function-test-checklist.md](function-test-checklist.md)。

對執行中的伺服器驗 `POST /mcp` 這一面，全程走真實路徑：真實帳號登入、真實
`issue_api_token` 核發 Token、官方 MCP 用戶端 SDK 連線、真實 `revoke_api_token`
撤銷。跑法：

```bash
cd scripts && bun install
LAYSI_USER=<系統管理員登入名稱> LAYSI_PASSWORD=<密碼> bun scripts/mcp-test.ts
MCP_URL=http://127.0.0.1:8089/mcp ... bun scripts/mcp-test.ts   # compose 的 mcp 服務
```

登入身分必須是系統管理員：`issue_api_token` 是 `AdminOnly`。

協定層由 `@modelcontextprotocol/sdk` 的 `StreamableHTTPClientTransport` 驅動，
握手、`tools/list`、`tools/call` 走的是 AI connector 用的同一套用戶端。畸形請求
（壞 body、缺欄位、偽造標頭）用戶端送不出來，那幾項另以 `fetch` 直接發。

唯一沒有走真實路徑的是 B5：沒有辦法在測試裡等過一天，所以它改寫 `created_at`
再驗過期，案例輸出會註明。

單元測試（`cargo test -p laysi-server`）驗純函式與 `#[ignore]` 的資料庫案例；
這份清單驗整條路徑：HTTP、認證、`dispatch` 五個步驟與稽核。兩邊不重疊。

## L 前置

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| L1 | `POST /v1/auth/login` | 200，拿到 session cookie，身分為系統管理員 |
| L2 | `issue_api_token` 核發七組測試 Token | 成功；此操作對 AI 是關閉的，畫面通道不受工具開關影響 |
| L3 | 明文的去向 | 資料庫只存 64 字元的 BLAKE3，稽核紀錄裡不出現明文 |

## A 協定

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| A1 | SDK 連線握手 | `serverInfo.name` 為 `laysi-one`、`capabilities.tools.listChanged` 為 false、`instructions` 非空 |
| A2 | 協定版本 | `2025-06-18` |
| A3 | `ping` | 有回應 |
| A4 | 未知 method | `-32601` |
| A5 | 缺少 method | `-32600` |
| A6 | body 不是 JSON | HTTP 400 |
| A7 | 請求 id | 字串與 `null` 都原樣帶回 |

## B 認證

身分只能來自 `api_token` 資料列。標頭說自己是誰，一律不算。

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| B1 | 沒有 `Authorization` | 401 |
| B2 | 錯誤的 Bearer | 401，SDK 連不上 |
| B3 | `kind = api` 的 Token | 401，MCP 只收 `kind = mcp` |
| B4 | `revoke_api_token` 撤銷 | 撤銷前可用、撤銷後立刻 401，稽核 `mcp_auth` 寫「已撤銷」 |
| B5 | 超過輪替期限 | 401，稽核寫「已過期」，`revoked_at` 仍為 NULL（本案例改寫 `created_at`） |
| B6 | 撤銷與過期的對外回應 | 兩者的狀態碼與訊息完全相同 |
| B7 | 偽造 `x-laysi-actor` / `x-laysi-role` | 呼叫照常執行，稽核記的是 Token 上的身分；偽造的名字一筆都不存在 |
| B8 | 壞 Token 加壞 body | 401 而不是 400：認證先於 body 解析 |
| B9 | 成功呼叫 | `last_used_at` 由 NULL 變成有值 |

## C Origin

streamable HTTP 規格要求的 DNS rebinding 防護。白名單是 `MCP_ALLOWED_ORIGINS`，預設空。

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| C1 | 不帶 `Origin` | 放行（正常的 AI connector 不帶這個標頭） |
| C2 | 帶 `Origin` 而白名單為空 | 403 |
| C3 | 帶 `Origin` 且不帶 Token | 403，Origin 檢查先於認證 |

## D 速率限制

`MCP_RATE_LIMIT_PER_MINUTE`（預設 60，0 為不限制），以 Token 編號分桶。
設為 0 時 D1、D2 略過。

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| D1 | 同一組 Token 一分鐘內呼叫 61 次 | 前 60 次通過，第 61 次回 429 與 `-32029` |
| D2 | 被擋下的那一次 | 寫稽核 `mcp_rate_limit`、`outcome = blocked`、`channel = mcp` |

## E tools/list

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| E1 | 工具數量 | 等於 `mcp_tool_grant` 中開著的筆數 |
| E2 | 關掉的工具 | 一個都不出現（讓模型看到又被拒絕是白跑一趟） |
| E3 | `description` | 都以 `[風險標籤]` 開頭 |
| E4 | `inputSchema` | SDK 收得下，且都是 `type: object` |
| E5 | 名稱集合 | 開關的總筆數等於 `/healthz` 回報的操作數，且開著的每一個都列得出來 |

## F tools/call

`dispatch` 的順序是工具開關 → 風險底線 → 角色權限 → 執行，被擋下的每一步都要寫稽核。

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| F1 | 開著又有權限的唯讀工具 | `isError: false`，`content[0].text` 是 JSON |
| F2 | 工具開關關著 | `isError: true`，訊息指向「MCP 與 AI」，稽核 `blocked` |
| F3 | 工具關著又沒有權限 | 回的是工具關閉的訊息，證明開關先於權限 |
| F4 | 角色沒有的權限 | `isError: true`，訊息寫「未獲授權」，稽核 `blocked` |
| F5 | 職員呼叫高風險系統工具 | 拒絕 |
| F6 | 拒絕訊息的用字 | 不出現操作代號（`get_dashboard`）與角色代號（`admin`／`boss`） |
| F7 | 不存在的工具名 | `isError: true`，且**不寫稽核**（沒有可歸屬的操作） |
| F8 | 缺 `params.name` | `-32600` |
| F9 | 輸入型別錯誤 | `isError: true`，不是協定層錯誤 |
| F10 | 完全不給 `arguments` | 視為全部使用預設值，呼叫成功 |
| F11 | 未連結員工的 Token 呼叫自助工具 | 拒絕，訊息說尚未連結員工資料 |

## G 稽核

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| G1 | 成功呼叫 | `channel = mcp`、`outcome = ok`、`actor` 與 `risk` 都對得上 |
| G2 | `_meta.prompt` | 原樣寫進 `prompt` 欄 |
| G3 | 超過 512 位元組的字串 | 換成長度說明，原文不進資料庫 |
| G4 | 同一組 Token 的所有紀錄 | 角色始終是核發當下那一個 |

## Z 收尾

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| Z1 | 測試 Token | 全部由 `revoke_api_token` 撤銷，撤銷後立刻失效 |

資料列保留不刪：撤銷是稽核事實。測試留下的 `ai_audit_log` 紀錄同樣保留，
`actor` 一律以「MCP 測試」開頭，事後認得出來。

## 角色矩陣

`scripts/mcp-role-matrix.ts` 把 7 個角色乘上對 AI 開著的每一個工具實際呼叫一次，
拿 `role_permission` 加上兩道身分底線算出的預期逐格比對。

```bash
LAYSI_USER=... LAYSI_PASSWORD=... bun scripts/mcp-role-matrix.ts
```

每個角色核發兩組 Token 輪流用，一組的呼叫數才不會撞上每分鐘的速率限制。
**有權限而且會寫入的工具不呼叫**，只記為未執行——那會真的過帳。

| 編號 | 檢查項目 | 期望 |
|---|---|---|
| INV1 | 帶身分底線的工具 | `admin_only` 與 `boss_only` 的 18 個對 AI 全部關閉，AI 碰不到 |
| INV2 | 各角色的 `tools/list` | 七份清單完全相同：工具清單只看工具開關，不看角色 |
| INV3 | 每一次拒絕 | 都在該身分底下留一筆 `outcome = blocked` 的稽核 |
| 逐格 | 應可執行的唯讀工具 | 不得回權限類錯誤 |
| 逐格 | 應被拒絕的工具 | 一定要回「未獲授權」／「限負責人執行」／「限系統管理員執行」 |

## 真實 AI 用戶端

腳本驗協定，這一段驗的是真的接得上 AI。核發一組 Token 之後：

```bash
claude mcp add --transport http --scope local laysi-one http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer <明文>"
claude mcp list          # 應顯示 ✔ Connected
claude mcp remove --scope local laysi-one
```

驗完記得 `revoke_api_token`。
