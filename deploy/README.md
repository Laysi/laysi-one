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
