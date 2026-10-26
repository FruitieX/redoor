import { expect, test } from "@playwright/test";
import fs from "node:fs/promises";
import path from "node:path";
import { $ } from "zx";
import {
    encodeFilesystemPath,
    setupTestDir,
    teardownTestDir,
    WEB_BASE_URL,
} from "./helpers";

test.describe("Diff layout", () => {
    let testDirPath = "";

    test.afterEach(async () => {
        if (testDirPath === "") {
            return;
        }

        await teardownTestDir(testDirPath);
        testDirPath = "";
    });

    test("switches edited lines between inline changes and separate lines and remembers it", async ({
        page,
    }) => {
        const ctx = await setupTestDir("diff-layout");
        testDirPath = ctx.testDirPath;
        const repositoryPath = path.join(ctx.testDirPath, "repository");
        const filePath = path.join(repositoryPath, "settings.ts");
        await fs.mkdir(repositoryPath);
        await fs.writeFile(filePath, 'export const mode = "committed";\n');
        await $`git -C ${repositoryPath} init`;
        await $`git -C ${repositoryPath} config user.email playwright@redoor.test`;
        await $`git -C ${repositoryPath} config user.name Playwright`;
        await $`git -C ${repositoryPath} add settings.ts`;
        await $`git -C ${repositoryPath} commit -m baseline`;
        await fs.writeFile(filePath, 'export const mode = "edited";\n');

        await page.goto(
            `${WEB_BASE_URL}/agents/${ctx.agentName}/browser/${encodeFilesystemPath(filePath)}?view=git`,
        );

        const diff = page.getByRole("region", { name: "Full Git diff" });
        const layout = page.getByRole("group", { name: "Diff layout" });

        const inline = layout.getByRole("button", {
            name: "Inline changes",
        });

        const separate = layout.getByRole("button", {
            name: "Separate lines",
        });

        const modifiedRows = diff.locator('tr[data-line-kind="modified"]');
        const deletedRows = diff.locator('tr[data-line-kind="delete"]');
        const insertedRows = diff.locator('tr[data-line-kind="insert"]');

        // Inline changes are the default, so an edited line reads as one row.
        await expect(inline).toHaveAttribute("aria-pressed", "true");
        await expect(modifiedRows).toHaveCount(1);
        await expect(modifiedRows.getByRole("deletion")).toHaveText(
            "committed",
        );
        await expect(modifiedRows.getByRole("insertion")).toHaveText("edited");
        await expect(deletedRows).toHaveCount(0);

        await separate.click();

        // The classic layout lists the removed and the added line as their own rows.
        await expect(separate).toHaveAttribute("aria-pressed", "true");
        await expect(modifiedRows).toHaveCount(0);
        await expect(deletedRows).toHaveText(
            /export const mode = "committed";/,
        );
        await expect(insertedRows).toHaveText(/export const mode = "edited";/);

        await page.reload();

        // The choice is stored in the browser, so it survives a reload.
        await expect(separate).toHaveAttribute("aria-pressed", "true");
        await expect(deletedRows).toHaveCount(1);
        await expect(insertedRows).toHaveCount(1);
        await expect(modifiedRows).toHaveCount(0);
    });
});
