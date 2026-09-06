#!/bin/sh
# Builds the body of a GitHub release. Takes the tag, writes markdown to stdout.
#
#   sh .github/release-notes.sh v0.1.5 > notes.md
#
# Separate from release.yml so wording can be checked by running it, and so an
# older release can be backfilled without cutting a tag.
#
# The page a customer lands on is the whole distribution: the compose file
# attached to it is what they run, and there is no installer and no
# documentation site. So it answers how to install and how to upgrade before
# it says what changed.

set -eu

tag=${1:?usage: release-notes.sh <tag>}
version=${tag#v}
repo=${GITHUB_REPOSITORY:-Laysi/laysi-one}
download="https://github.com/$repo/releases/download/$tag/docker-compose.prod.yml"
previous=$(git tag --list 'v*' --sort=-v:refname | grep -A1 -x "$tag" | tail -n1)

cat <<EOF
Laysi One $version。台灣中小企業的財務與人事系統。

線上試用：<https://demo.laysi.one>（每天凌晨重建，資料為示範用）

## 安裝

\`\`\`bash
curl -LO $download
docker compose -f docker-compose.prod.yml up -d
docker compose -f docker-compose.prod.yml logs secrets   # 印出初始化代碼
\`\`\`

瀏覽器開 <http://localhost:8088>，輸入初始化代碼與經銷商提供的啟動金鑰，建立第一個帳號。
資料庫密碼與其餘密鑰由 compose 自己產生，沒有 \`.env\` 要填。

## 升級

\`\`\`bash
curl -LO $download
docker compose -f docker-compose.prod.yml up -d
\`\`\`

資料留在 volume，schema 由伺服器啟動時自己套用。**先備份 \`pgdata\` 與 \`rustfsdata\`**：
版本往前走得了，資料庫回不去。

## 映像

\`\`\`
ghcr.io/laysi/laysi-one-api:$version
ghcr.io/laysi/laysi-one-web:$version
ghcr.io/laysi/laysi-one-backup:$version
\`\`\`

## 本版變更
EOF

# Commit subjects, not merged pull requests: this repository commits to main.
#
# `license-server/` is excluded by path. It is a second deployable in the same
# repository, it ships on its own `license-v*` tags, and no customer reading
# this page runs it -- but its history arrived here in one merge, so without
# this the first release after that merge lists every commit it ever had.
range=$tag
[ -n "$previous" ] && [ "$previous" != "$tag" ] && range="$previous..$tag"
#
# `ci:` and `chore:` are dropped by type. Nothing a customer runs changes when
# a workflow does, and a changelog that lists them buries the lines that do.
git log --no-merges --pretty='- %s' "$range" -- . ':(exclude)license-server' |
    grep -vE '^- (ci|chore)(\([^)]*\))?:' |
    head -40

if [ "$range" != "$tag" ]; then
    printf '\n**完整變更**：https://github.com/%s/compare/%s...%s\n' "$repo" "$previous" "$tag"
fi

printf '\n## 授權\n\n專有商業授權，見 `LICENSE.txt`。安裝需要經銷商核發的啟動金鑰。\n'
