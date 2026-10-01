# 示範站

`demo.laysi.one` 與 `one.laysi.io` 在同一台主機，同一份映像、同一份
`docker-compose.prod.yml`，差別只有 `docker-compose.demo.yml` 這層覆蓋與資料的處置：
正式站跨部署保留帳本，示範站每天凌晨四點（台北）整份丟掉重建。

推一個 `v*` 標籤就會部署到示範站；正式站維持手動
（`gh workflow run deploy.yml -f version=0.1.0`）。

## 主機上要先準備的兩件事

這兩件事 CI 做不到。第二件缺了部署會停下來並說出缺什麼，第一件由重建腳本自己擋。

1. **`curl`**：重建時要呼叫一次 `/v1/auth/bootstrap`。其餘只用到 podman。
2. **`~/.config/laysi-demo.env`**，systemd 單元讀它：

   ```sh
   DEMO_ACTIVATION_KEY=LAYSI-XXXXX-XXXXX-XXXXX-XXXXX
   DEMO_PASSWORD=laysi-demo-2026
   ```

   金鑰在 `license.laysi.one` 開一張授權給示範站取得。**沒有金鑰示範站會是唯讀的**：
   全新的資料庫沒有寬限期，而未授權的安裝不接受任何寫入，那正是評估最需要的東西。

## 重建做了什麼

`deploy/reset-demo.sh`：丟掉 `pgdata` 與 `rustfsdata`（**不動 `secrets`**，簽章之外
資料庫密碼與初始化代碼也在那裡）、以新映像起來、呼叫 `/v1/auth/bootstrap` 建
`admin` 並啟用授權、建 `boss`／`hr`／`acct`、跑 `seed-demo` 種帳本與 `demo` 職員帳號，
最後裝上一個把這五個帳號凍結的 trigger。

五個帳號共用一組密碼，寫在邀請信裡，不印在畫面上。

部署會等這次重建跑完，而且 api 容器換成這次的映像才算成功。重建的輸出寫進主機上的
`~/laysi-one-demo/reset.log`，每次重建從頭寫起，只留最近一次；重建失敗時部署跟著失敗，
並印出它的最後 40 行。不寫進 journal：部署帳號在這台主機讀不到自己的 journal。

## 那個 trigger

`demo_freeze_shared_login` 只認那五個帳號名，改密碼、停用、換角色與刪除一律還原。
沒有它，早上十點有人改掉 `boss` 的密碼，當天其餘受邀的人就進不來，要等到隔天四點。

**只綁那五個名字**是刻意的：訪客自己建的帳號照常改得動密碼、停得掉、刪得掉，
所以帳號管理那幾個畫面示範的仍然是真的行為。

## 已知的兩件事

- 每晚重建會換掉 `install_id`，授權伺服器的連續性 nonce 因此對不上，示範站那張授權
  的清單上會累積計數。**那是設計上不擋的**（見 `license-server/CLAUDE.md` 決定 3），
  重建不是複製安裝。主機識別碼取自 `/etc/machine-id`，不隨重建改變，所以綁定仍然成立。
- MCP 跑著但沒有任何對外的入口：`docker-compose.demo.yml` 不發佈埠、也不接上
  `proxy` 網路，tunnel 沒有東西可以指。要開放就加一條 ingress，不必改設定。

## 登入畫面上的帳密提示

由 Cloudflare Worker `laysi-demo-credentials` 注入，路由 `demo.laysi.one/*`，
原始碼是 `deploy/demo-credentials-worker.js`。**產品程式裡沒有這件事**：一個共用的評估
安裝是部署上的安排，不是客戶要帶著、要設定、要關掉的功能，為它在 `web/src` 加一個
旗標等於讓每一份安裝都認得「demo」這個字。

面板只在畫面上有密碼欄位時出現，也就是登入頁。工作階段 cookie 是 HttpOnly，JS 讀不到，
問畫面是唯一的判斷方式，而且比綁一個 class 名稱更禁得起改版。

同一支 Worker 對 HTML 回應加上 `X-Robots-Tag: noindex, nofollow`：爬蟲在這裡只到得了
登入頁，而那一頁印著共用密碼，被索引還會在品牌字上跟 `laysi.one` 相爭。

改了原始碼要自己上傳，沒有 CI：

```bash
curl -X PUT \
  "https://api.cloudflare.com/client/v4/accounts/<account>/workers/scripts/laysi-demo-credentials" \
  -H "Authorization: Bearer <token>" \
  -F 'metadata={"main_module":"worker.js","compatibility_date":"2026-09-01"};type=application/json' \
  -F 'worker.js=@deploy/demo-credentials-worker.js;type=application/javascript+module'
```

Snippets 是更輕的工具，但這個 zone 沒有開放（API 回 `snippets are not allowed`）。

**密碼印在頁面上不是風險**：那五個帳號由 `demo_freeze_shared_login` 凍結，看得到也改不掉，
而且每晚整份重建。
