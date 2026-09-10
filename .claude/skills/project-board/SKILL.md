---
name: project-board
description: Laysi/laysi-one GitHub 看板的專案政策 overlay。凡是「開始／完成一項工作、發現新待辦或技術債、建 issue、拆 sub-issue、改 Status、設 Priority、寫驗收條件、連結 PR/commit、標記 blocker、寫 implementation note、產生下一步 task」都必須依本 skill 操作（CLAUDE.md 定義為完成定義的一部分）。執行機制＝github-loop-engineering-skill（GitHub MCP 優先）；本 skill 提供 laysi-one 專屬座標、語意約定、內文格式、close ritual 與 gh CLI fallback recipe。
---

# laysi-one 看板操作手冊

看板同步是必要步驟，政策規則在 `CLAUDE.md`「專案管理」章節。**工作項一律用真 issue**（不是 draft 卡）——只有 issue 能用 sub-issue、labels、type、PR/commit 連結、依賴標記。尚未成形的純想法才允許用 draft 卡暫存。

## 執行機制：github-loop-engineering-skill

看板操作的執行引擎是 vendored skill `github-loop-engineering-skill`（Discover → Frame → Act → Verify → Persist → Continue/Stop，GitHub MCP-only，寫後必讀回驗證）。做任何看板操作前先載入該 skill；本 skill 只補 laysi-one 專屬的政策與參數，兩者衝突時**以本 skill（repo 政策）為準**。已知的 repo 政策覆寫：

- **內文格式用本 repo 的七段中文結構**（見下方「issue 內文格式」），覆寫 loop skill 的 `references/work-item-format.md`。那份參考自己也寫著 repo 既有格式優先。
- **不新增 label 分類**：laysi-one 只用現有五個 label（見下方「語意約定」）。要新增分類先問使用者，不要自行建立。
- **分級只有看板 Priority 欄位一軌**，沒有 `priority:*` label。
- **建 issue 時 assignee 設為自己**。MCP 路徑：`@me` 是 gh CLI 專屬簡寫，`issue_write.assignees` 不吃——先 `get_me` 解析出 login 再帶入；gh fallback 才用 `-a "@me"`。
- **MCP 缺 projects 工具時不停在回報**：看板同步是必要步驟，MCP 做不到的欄位操作允許走本 skill 的 gh CLI fallback（先回報再 fallback，不是靜默繞過）。

### 工具選擇

- **Issue 層（建/讀/改/關、comment、sub-issue、type、label）→ GitHub MCP**：`issue_write`／`issue_read`／`add_issue_comment`／`sub_issue_write`／`list_issue_types`／`list_label`／`search_issues`。payload 範式見 loop skill 的 `references/github-mcp-tools.md`。
- **Project 看板欄位（Status/Priority、掛板）→ 依 session 實況**：
  - 若 MCP schema 有 `projects_list`／`projects_get`／`projects_write`（`.mcp.json` 的 github entry 已帶 `"X-MCP-Toolsets": "context,repos,issues,labels,projects,pull_requests"`）→ 走 MCP，欄位與選項用名稱解析（`updated_field: {name:"Status", value:"Triage"}`），**不快取 id**。
  - 若 projects 工具不在（token 缺 `project` scope，或 toolset 未生效）→ 走文末 **gh fallback recipe**。

## 固定座標

Repo `Laysi/laysi-one`（private）。看板：owner `Laysi`（org）；**number 執行時用 `projects_list` 現查，本檔不記**——記下來就是多一份會漂走的副本。node id、欄位與選項 id 一律現查。

## 語意約定

- **Status**：**Triage＝AI 產生、尚未經人工審核**——agent 自行發現或建立的 issue 一律停在這裡；**Todo＝使用者核准過、真正可啟動的工作**。把卡移出 Triage（核准進 Todo，或 close 不做）是**使用者的動作，agent 不得自行升級**；使用者明示指派的工作才可直接進 Todo 或 In Progress。
- **Priority**：P0＝生產事故或阻斷，立即處理並主動告知使用者；P1＝當前重點；P2＝backlog。未設＝尚未分級，建 issue 時必設。**只設看板欄位，不設 label**——laysi-one 沒有 `priority:*` label，不要為了分級去建。
- **Issue type**：repo 已啟用三種，現有 issue 全部設過：**Feature**＝新能力；**Bug**＝行為錯誤或資安漏洞；**Task**＝其餘（重構、驗證、維運、決策、文件）。MCP `issue_write` 直接帶 `type`。
- **Labels**：只用 repo 現有的五個——`bug`／`enhancement`／`documentation`／`ui`／`ci/cd`。慣例是 type 與 label 對齊（Bug 配 `bug`、Feature 配 `enhancement`），畫面改動加 `ui`，文件改動加 `documentation`，CI 與 workflow 改動加 `ci/cd`；一張可以多個。**新分類要先問使用者**，不要自行 `create_label`。改 label 時記得 MCP `issue_write` 的 `labels` 是**整組替換**——先讀全集再送完整清單。
- **語言**：issue 標題與內文 zh-TW（技術名詞、識別字、指令保留英文）；commit message 英文，沿用 repo 的 conventional commits（`fix(license): …`）。

## issue 內文格式

七段，順序固定，空的段落整段省略而不是留占位。現成範例是 issue #7。

```markdown
## 成果

一句到一段，寫這件事做完之後看得到什麼。

## 背景

為什麼要做、現在的行為是什麼，以及冷啟動所需的 repo 上下文。**指到檔案與行號**
（`web/src/screens/WorkCalendar.tsx:31-32`），讓沒有上下文的人直接接得上。

## 範圍

### 包含

- 具體改動

### 不包含

- 明確的邊界

## 驗收條件 (Acceptance Criteria)

- [ ] 可觀察的行為或產物
- [ ] 明確的驗證指令與預期結果

## 驗證

- `指令`：預期訊號
- 手動：預期行為

## 附記

這張議題唯一的取捨、或事後回頭會想知道的決定。

## 預估

半天／一天／三天。
```

驗收條件每一條都要**獨立可驗證**，不寫「正常運作」「測試通過」這種沒有指名預期行為的句子。對應本 repo 的驗證 gate：

- 後端：`cargo test --workspace`、`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`
- 前端：`npm run typecheck`、`npm run build`
- 邊界檢查：`bun scripts/schema-test.ts`、`format-test.ts`、`nav-test.ts`、`markdown-test.ts`、`mcp-role-matrix.ts`
- 端對端（需要容器起著）：`bun scripts/function-test.ts && bun scripts/schema-live-test.ts`
- 改過 `laysi-ops`：`cargo run -p laysi-server --bin laysi-schema -- ../web/src/api/generated.ts` 產出無 diff

### blocker 段

卡住時加上這一段，不要改寫內文其他部分：

```markdown
## 卡在

- 阻礙：Laysi/laysi-one#123，或精確的外部原因
- 起始：YYYY-MM-DD
- 解除條件：具體的事件或證據
```

解除時刪掉已解決的項目、保留未解決的，並在 comment 記錄解法。整段刪掉的條件是一個 blocker 都不剩。

## 建立 issue

1. **先查重**（`search_issues`）。
2. `issue_write` create：zh-TW 標題、七段內文、`type` ＋ label ＋ assignee＝自己的 login（先 `get_me` 取得；`@me` 在 MCP 無效）。**type 與 label 兩者必齊。**
3. 掛板並設欄位：MCP `projects_write`（`add_project_item` → `update_project_item`）或 gh fallback。看板若有 auto-add workflow 會搶掛（非同步）——「already exists」視為成功，找到既有 item 續設欄位。
4. Status：AI 自行發現的工作→**Triage**；使用者明示指派→Todo 或直接 In Progress。Priority 欄位同時設。
5. **讀回驗證** issue 與看板欄位（loop skill 的 Verify 步驟，勿省）。

## 拆 sub-issue

超過約一天的工作、或含多個可獨立驗收的部分 → 拆 sub-issue。子項是普通 issue（走上節完整流程，含掛板），再用 `sub_issue_write`（`add`）掛父子——`sub_issue_id` 用子項的**數字 database ID**（create/read 回應的 `id`），不是 issue number 也不是 node id。小於半天的細項用父 issue 內文的驗收條件 checklist 即可。

## 更新 Status

- 移動時機：核准→Todo（**只由使用者做**）；開工→In Progress；卡住→Blocked（同時寫 blocker 段）；完成→Done。
- **內建 workflows 執行時確認，不要假設**：看板可能啟用 `Auto-add to project`、`Item closed`（close→自動移 Done）、`Auto-close issue`（Status 改 Done→自動關 issue）等。哪幾條開著由 `projects_get` 或看板設定頁回答；沒確認之前不要把 Status 設成 Done 當作「順手」的動作，那可能連帶把 issue 關掉。自動化不可假設生效：close 之後仍要讀回看板確認 Done。
- 每次移動 Status 都順手把內文的「背景」更新為當下事實。

## 驗收條件的維護

完成一條勾一條（`issue_write` update 送**完整**內文，只改該 checkbox）；**全部勾完才允許 close**。不因為程式碼寫完就勾——勾的依據是那條的驗證跑過了。

## implementation note

放在 comment，不改內文：

```markdown
**Implementation note (YYYY-MM-DD)：** 決定、發現或進度。證據：PR／commit／路徑／測試。剩下：具體的下一步。
```

值得寫的是耐久的決定、不明顯的陷阱、交接到一半的狀態、驗證結果、PR 或 commit 連結。例行敘述不寫。

## close ritual

1. 驗收條件全部勾完，每一條都有跑過的證據。
2. 補一則完成 comment：

```markdown
**完成 (YYYY-MM-DD)**

- 成果：改了什麼
- 變更：PR、commit 或產物連結
- 驗證：指令與觀察到的結果
- 範圍變動：無，或明確的縮減決定
- 後續：issue 連結、提案，或無
```

3. close issue，然後**讀回看板確認 Status 是 Done**（自動化可能沒生效）。`Item reopened` 這類 workflow 若未啟用，reopen 之後要手動把 Status 移回。
4. 掃描下一步：這次改動有沒有帶出新的待辦或技術債，有就建 issue 停在 Triage。

## gh CLI fallback

MCP 的 projects 工具不在時走這條。**先回報再 fallback。**

現況：`gh` 已登入（帳號 `jerry830401`），issue 與 label 馬上可用，但看板欄位需要補 scope。
請使用者在 Claude Code 裡跑（`!` 是非互動模式，少了 `-h` 會直接報錯；agent 自己完成不了瀏覽器授權）：

```bash
! gh auth refresh -h github.com -s read:project,project
```

`.mcp.json` 走的是另一組 token（環境變數 `GITHUB_PERSONAL_ACCESS_TOKEN`），要 MCP 的 projects 工具出得來，那組 PAT 也要勾 `project` scope。

看板 number 與欄位 id 現查：

```bash
gh project list --owner Laysi
gh project field-list <number> --owner Laysi --format json
gh project item-list <number> --owner Laysi --format json
```

掛板與設欄位：

```bash
gh project item-add <number> --owner Laysi --url https://github.com/Laysi/laysi-one/issues/<N>
gh project item-edit --project-id <PVT_id> --id <PVTI_id> \
  --field-id <PVTSSF_id> --single-select-option-id <option_id>
```

id 一律當場從 `field-list`／`item-list` 讀出來，不要抄進本檔——它們換一個看板就全錯。
