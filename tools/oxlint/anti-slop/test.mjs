import { glob } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// Keep upstream RuleTester cases executable without adding them to Vitest's application suites.
let suites = 0;
for await (const file of glob("**/*.rule-test.ts", { cwd: import.meta.dirname })) {
    await import(pathToFileURL(join(import.meta.dirname, file)).href);
    suites += 1;
}
console.log(`Passed ${suites} anti-slop rule test files.`);
