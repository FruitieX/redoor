import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { $ } from "zx";

// Exercise the registered compatibility wrapper across files, where RuleTester cannot catch leaked state.
const directory = await mkdtemp(join(tmpdir(), "redoor-anti-slop-cli-"));
const config = join(directory, "oxlint.json");
const first = join(directory, "first.ts");
const second = join(directory, "second.ts");
await writeFile(config, JSON.stringify({
    jsPlugins: [{
        name: "anti-slop",
        specifier: fileURLToPath(new URL("./index.ts", import.meta.url)),
    }],
    rules: { "anti-slop/require-readable-spacing": "error" },
}));
await writeFile(first, `export const a = 1;
/** Attached to b. */
export const b = 2;
`);
await writeFile(second, `export function f() {
const a = 1;
const b = 2;
return a + b;
}
`);
const run = $({ quiet: true, nothrow: true });
const rejected = await run`mise exec -- pnpm exec oxlint --config ${config} ${first} ${second}`;
// A real CLI invocation must load the plugin and reject missing structural spacing.
assert.equal(rejected.exitCode, 1, rejected.toString());
assert.match(rejected.toString(), /require-readable-spacing/);
const fixed = await run`mise exec -- pnpm exec oxlint --config ${config} --fix ${first} ${second}`;
// Fixing both files together must retain documentation attachment and local binding groups.
assert.equal(fixed.exitCode, 0, fixed.toString());
assert.equal(await readFile(first, "utf8"), `export const a = 1;

/** Attached to b. */
export const b = 2;
`);
assert.equal(await readFile(second, "utf8"), `export function f() {
const a = 1;
const b = 2;

return a + b;
}
`);
const stable = await Promise.all([readFile(first, "utf8"), readFile(second, "utf8")]);
const clean = await run`mise exec -- pnpm exec oxlint --config ${config} ${first} ${second}`;
// The fixed output must lint cleanly without relying on another fix pass.
assert.equal(clean.exitCode, 0, clean.toString());
const repeated = await run`mise exec -- pnpm exec oxlint --config ${config} --fix ${first} ${second}`;
// Repeated fixes must be stable across files.
assert.equal(repeated.exitCode, 0, repeated.toString());
assert.deepEqual(await Promise.all([readFile(first, "utf8"), readFile(second, "utf8")]), stable);
