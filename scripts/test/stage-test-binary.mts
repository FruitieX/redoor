import { chmod, copyFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { $ } from "zx";

const PROJECT_ROOT = fileURLToPath(new URL("../../", import.meta.url));

/**
 * Copies the debug build to `destination` without its debug info.
 *
 * About 85% of the debug binary is debug info, and tests move the staged binary around:
 * SSH provisioning uploads it to every fresh remote app directory, upgrade tests archive
 * it and agent upgrades push it. Stripping only the debug sections keeps the symbol table,
 * so panics and backtraces still name functions. The staged copy is also immutable, so
 * concurrent Cargo builds cannot replace it mid-run.
 */
export async function stageTestBinary(destination: string): Promise<void> {
    const source = join(PROJECT_ROOT, "target/debug/redoor");

    // `-S` strips only debug sections on both GNU and BSD strip.
    const stripped = await $`strip -S -o ${destination} ${source}`
        .quiet()
        .nothrow();

    if (stripped.exitCode !== 0) {
        // A missing or incompatible strip only costs speed, so fall back to the full binary.
        await copyFile(source, destination);
    }

    await chmod(destination, 0o755);
}
