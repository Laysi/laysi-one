#!/usr/bin/env bun
// Which zod schema belongs to which operation, read out of the call sites.
//
// callOp's fourth argument is the schema the screens validate against, so the
// screens already hold the answer. Reading it back beats keeping a second list
// that has to be remembered whenever a screen changes.
//
// Used by scripts/function-test.ts, which checks the write operations while it
// drives them, and by scripts/schema-live-test.ts, which checks the reads.

import { readFileSync, globSync } from "node:fs";

const WEB = new URL("../web", import.meta.url).pathname;

/// Arguments are split on top-level commas, which is enough for
/// `callOp('name', {...}, undefined, schema)`. Quoted text is skipped, so a
/// comma inside a string does not look like an argument break.
function wiring(): Map<string, Set<string>> {
  const found = new Map<string, Set<string>>();
  for (const file of globSync(`${WEB}/src/**/*.{ts,tsx}`)) {
    const src = readFileSync(file, "utf8");
    let at = src.indexOf("callOp(");
    while (at >= 0) {
      let depth = 0;
      const start = at + "callOp".length;
      const args: string[] = [];
      let argStart = start + 1;
      let quote = "";
      for (let i = start; i < src.length; i += 1) {
        const c = src[i]!;
        if (quote) {
          if (c === "\\") i += 1;
          else if (c === quote) quote = "";
          continue;
        }
        if (c === "'" || c === '"' || c === "`") quote = c;
        else if (c === "(" || c === "{" || c === "[") depth += 1;
        else if (c === ")" || c === "}" || c === "]") {
          depth -= 1;
          if (depth === 0) {
            args.push(src.slice(argStart, i));
            break;
          }
        } else if (c === "," && depth === 1) {
          args.push(src.slice(argStart, i));
          argStart = i + 1;
        }
      }
      // A multi-line call ends with a trailing comma, which leaves an empty
      // final argument behind.
      if (args.length > 0 && args[args.length - 1]!.trim() === "") args.pop();
      const name = /^\s*'(\w+)'\s*$/.exec(args[0] ?? "");
      const schema = /^\s*(\w+)\s*$/.exec(args[3] ?? "");
      if (args.length === 4 && name && schema) {
        const slot = found.get(name[1]!) ?? new Set<string>();
        slot.add(schema[1]!);
        found.set(name[1]!, slot);
      }
      at = src.indexOf("callOp(", at + 1);
    }
  }
  return found;
}

/// Operations whose response shape depends on their input, and the extra
/// schema that covers the other shape.
///
/// `list_notices` and `list_my_notices` answer with one notice's every version
/// when given an id, and with the list otherwise. `components/NoticeHistory`
/// validates that with `noticeHistory`, but it is written once for both
/// operations and passes the name in a variable, so reading the call sites
/// cannot attribute it.
const ALSO_ACCEPTS: Record<string, string> = {
  list_notices: "noticeHistory",
  list_my_notices: "noticeHistory",
};

const client = (await import(`${WEB}/src/api/client.ts`)) as Record<string, any>;
const byOp = wiring();
for (const [op, name] of Object.entries(ALSO_ACCEPTS)) {
  if (byOp.has(op)) byOp.get(op)!.add(name);
}

export const wiredOperations = [...byOp.keys()].sort();

/// Every schema this operation's response is allowed to match.
///
/// Usually one, and more only where the shape depends on the input, in which
/// case matching any of them passes. The call site itself has no such
/// ambiguity: there the schema and the return type are the same argument.
function schemasFor(op: string): { name: string; schema: { safeParse: (v: unknown) => any } }[] {
  return [...(byOp.get(op) ?? [])].map((name) => {
    const schema = client[name];
    if (!schema?.safeParse) throw new Error(`${op} 指向的 ${name} 不是 client.ts 匯出的 schema`);
    return { name, schema };
  });
}

/// The first few problems, as one line, or null when the response matches.
export function mismatch(op: string, data: unknown): string | null {
  const declared = schemasFor(op);
  if (declared.length === 0) return null;
  const failures: string[] = [];
  for (const { name, schema } of declared) {
    const parsed = schema.safeParse(data);
    if (parsed.success) return null;
    failures.push(
      `${name}：` +
        parsed.error.issues
          .slice(0, 3)
          .map((i: any) => `${i.path.join(".")} ${i.message}`)
          .join("；"),
    );
  }
  return failures.join(" / ");
}
