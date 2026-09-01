#!/usr/bin/env bun
// Build the manifest `refresh_regulations` reads.
//
// The TOML stays the source: it is what a person edits, what the calculators
// are tested against, and what save_regulation's probe() validates. This
// collects every rule file into one JSON document, which the publishing
// workflow puts on the feed so an existing installation receives an amended
// rate without a new image.
//
// Usage: bun scripts/regulation-manifest.ts [out.json]
//
// The `file` key is what reg_update remembers, so it must stay stable once
// published: change it and every installation applies the amendment a second
// time, including the ones that deleted it on purpose. It carries the .sql
// suffix the shipped updates used before the feed existed, so an install that
// already applied one does not receive it again.

import { readdirSync } from 'node:fs';
import { basename, join } from 'node:path';

const root = join(import.meta.dir, '..', 'backend', 'regulation');
const out = Bun.argv[2] ?? join(import.meta.dir, '..', 'manifest.json');

interface Rule {
  domain: string;
  version: string;
  effective_from: string;
  effective_to?: string;
  source_note: string;
  payload: unknown;
}

// A date parses out of TOML as a Date; the rest are strings the payload keeps
// verbatim, because the calculators parse them and the screen edits them.
const day = (v: unknown) => (v instanceof Date ? v.toISOString().slice(0, 10) : String(v));

const updates: Rule[] = [];
for (const year of readdirSync(root).sort()) {
  for (const name of readdirSync(join(root, year)).sort()) {
    if (!name.endsWith('.toml')) continue;
    const rel = `${year}/${name}`;
    const raw = Bun.TOML.parse(await Bun.file(join(root, rel)).text()) as Record<string, unknown>;
    for (const key of ['domain', 'version', 'effective_from', 'source_note', 'payload']) {
      if (raw[key] === undefined) throw new Error(`${rel} 少了 ${key}`);
    }
    const from = day(raw.effective_from);
    updates.push({
      file: `${from}_${basename(name, '.toml')}.sql`,
      domain: String(raw.domain),
      version: String(raw.version),
      effective_from: from,
      effective_to: raw.effective_to === undefined ? null : day(raw.effective_to),
      source_note: String(raw.source_note),
      payload: raw.payload,
    } as Rule);
  }
}

const seen = new Set<string>();
for (const u of updates) {
  if (seen.has(u.file)) throw new Error(`${u.file} 重複，同一天同一個領域只能有一版`);
  seen.add(u.file);
}

await Bun.write(out, `${JSON.stringify({ updates }, null, 2)}\n`);
console.log(`已寫入 ${out}：${updates.length} 版`);
