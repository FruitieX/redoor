import { expect, test, type Locator } from "@playwright/test";
import fs from "node:fs/promises";
import path from "node:path";
import { $ } from "zx";
import { ApiClient } from "#ui/api-client";
import { userStateSchema } from "#ui/user-state";
import {
    API_BASE_URL,
    encodeFilesystemPath,
    setupTestDir,
    teardownTestDir,
    WEB_BASE_URL,
} from "./helpers";

/** Whether the diff table is wider than its box, so the reader would scroll it sideways. */
async function scrollsSideways(table: Locator) {
    return table.evaluate((element) => {
        const box = element.parentElement;

        return box !== null && box.scrollWidth > box.clientWidth;
    });
}

test.describe("Diff layout", () => {
    let testDirPath = "";

    test.afterEach(async () => {
        // Later tests expect diffs to scroll sideways, the default.
        const api = new ApiClient(API_BASE_URL);
        await api.login("test-user", "test-password");
        const state = userStateSchema.parse((await api.getUserState()).state);

        if (state.wrapDiffLines) {
            await api.updateUserState({
                state: { ...state, wrapDiffLines: false },
            });
        }

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

    test("wraps long diff lines on request and remembers it", async ({
        page,
    }) => {
        const ctx = await setupTestDir("diff-wrap");
        testDirPath = ctx.testDirPath;
        const repositoryPath = path.join(ctx.testDirPath, "repository");
        const filePath = path.join(repositoryPath, "long.ts");
        const longValue = "word ".repeat(200);
        await fs.mkdir(repositoryPath);
        await fs.writeFile(filePath, `export const a = "${longValue}";\n`);
        await $`git -C ${repositoryPath} init`;
        await $`git -C ${repositoryPath} config user.email playwright@redoor.test`;
        await $`git -C ${repositoryPath} config user.name Playwright`;
        await $`git -C ${repositoryPath} add long.ts`;
        await $`git -C ${repositoryPath} commit -m baseline`;
        await fs.writeFile(filePath, `export const b = "${longValue}";\n`);

        const api = new ApiClient(API_BASE_URL);
        await api.login("test-user", "test-password");

        await page.goto(
            `${WEB_BASE_URL}/agents/${ctx.agentName}/browser/${encodeFilesystemPath(filePath)}?view=git`,
        );

        const diff = page.getByRole("region", { name: "Full Git diff" });
        const table = diff.getByRole("table", { name: "Changes in long.ts" });
        const wrap = page.getByRole("button", { name: "Wrap diff lines" });

        // Wrapping is off by default, so a long line overflows its box.
        await expect(wrap).toHaveAttribute("aria-pressed", "false");
        await expect(table).toBeVisible();
        expect(await scrollsSideways(table)).toBe(true);

        await wrap.click();

        // Wrapping fits the long line inside the box, so nothing scrolls sideways.
        await expect(wrap).toHaveAttribute("aria-pressed", "true");
        await expect.poll(() => scrollsSideways(table)).toBe(false);

        // The toggle is saved to the server-side user state, not just this page.
        await expect
            .poll(
                async () =>
                    userStateSchema.parse((await api.getUserState()).state)
                        .wrapDiffLines,
            )
            .toBe(true);

        await page.reload();

        // The stored preference applies again after a reload.
        await expect(wrap).toHaveAttribute("aria-pressed", "true");
        await expect.poll(() => scrollsSideways(table)).toBe(false);
    });
});
