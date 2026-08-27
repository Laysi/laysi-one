#!/usr/bin/env bun
// Turn a factory rule file into a shipped amendment.
//
// The TOML stays the source: it is what a person edits, what the calculators
// are tested against, and what save_regulation's probe() validates. This
// writes the same payload into backend/regulation-updates/ so an existing
// installation receives it on the next start, because seed_regulations()
// only fills an empty table.
//
// Usage: bun scripts/regulation-sql.ts 2027/labor_insurance.toml
//
// After writing the file, list it in Store::REGULATION_UPDATES.

import { parseArgs } from 'node:util';
import { basename, join } from 'node:path';

const root = join(import.meta.dir, '..', 'backend');
const { positionals } = parseArgs({ args: Bun.argv.slice(2), allowPositionals: true });
const rel = positionals[0];
if (!rel) {
  console.error('用法：bun scripts/regulation-sql.ts <年度>/<domain>.toml');
  process.exit(2);
}

const source = join(root, 'regulation', rel);
const raw = Bun.TOML.parse(await Bun.file(source).text()) as {
  domain: string;
  version: string;
  effective_from: string;
  effective_to?: string;
  source_note: string;
  payload: unknown;
};

for (const key of ['domain', 'version', 'effective_from', 'source_note', 'payload'] as const) {
  if (raw[key] === undefined) throw new Error(`${rel} 少了 ${key}`);
}

// A date parses out of TOML as a Date; the rest are strings the payload keeps
// verbatim, because the calculators parse them and the screen edits them.
const day = (v: unknown) => (v instanceof Date ? v.toISOString().slice(0, 10) : String(v));
const lit = (v: string) => `'${v.replaceAll("'", "''")}'`;
const nullable = (v: unknown) => (v === undefined || v === null ? 'NULL' : lit(day(v)));

const out = join(root, 'regulation-updates', `${day(raw.effective_from)}_${basename(rel, '.toml')}.sql`);
await Bun.write(
  out,
  `-- ${raw.domain} ${raw.version}，${raw.source_note}
-- 由 scripts/regulation-sql.ts 從 regulation/${rel} 產出，不要手改。
--
-- DO NOTHING：這一版已經由管理員自己建過就不動它。

INSERT INTO reg_ruleset (domain, version, effective_from, effective_to, source_note, payload)
VALUES (
    ${lit(raw.domain)},
    ${lit(raw.version)},
    ${lit(day(raw.effective_from))},
    ${nullable(raw.effective_to)},
    ${lit(raw.source_note)},
    ${lit(JSON.stringify(raw.payload))}::jsonb
)
ON CONFLICT (domain, version) DO NOTHING;
`,
);

console.log(`已寫入 ${out}`);
console.log('請加進 laysi-store 的 Store::REGULATION_UPDATES。');
