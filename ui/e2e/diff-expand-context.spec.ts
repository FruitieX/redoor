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

/** Numbered lines with one of them edited, so the folds have predictable sizes. */
function numberedLines(count: number, changed: number | null) {
    return Array.from({ length: count }, (_, index) =>
        index + 1 === changed ? `edited ${index + 1}\n` : `line ${index + 1}\n`,
    ).join("");
}

test.describe("Diff context expansion", () => {
    let testDirPath = "";

    test.afterEach(async () => {
        if (testDirPath === "") {
            return;
        }

        await teardownTestDir(testDirPath);
        testDirPath = "";
    });

    test("reveals folded lines and the whole file without fetching again", async ({
        page,
    }) => {
        const ctx = await setupTestDir("diff-expand-context");
        testDirPath = ctx.testDirPath;
        const repositoryPath = path.join(ctx.testDirPath, "repository");
        const filePath = path.join(repositoryPath, "notes.txt");
        await fs.mkdir(repositoryPath);
        await fs.writeFile(filePath, numberedLines(60, null));
        await $`git -C ${repositoryPath} init`;
        await $`git -C ${repositoryPath} config user.email playwright@redoor.test`;
        await $`git -C ${repositoryPath} config user.name Playwright`;
        await $`git -C ${repositoryPath} add notes.txt`;
        await $`git -C ${repositoryPath} commit -m baseline`;
        await fs.writeFile(filePath, numberedLines(60, 30));

        let diffRequests = 0;

        page.on("request", (request) => {
            if (request.url().includes("/git/diff")) {
                diffRequests++;
            }
        });

        await page.goto(
            `${WEB_BASE_URL}/agents/${ctx.agentName}/browser/${encodeFilesystemPath(filePath)}?view=git`,
        );

        const diff = page.getByRole("region", { name: "Full Git diff" });
        const folds = diff.locator("tr[data-skip-block]");

        const row = (line: number) =>
            diff.locator(`tr[data-line-new="${line}"]`);

        // Only the edit and three lines around it show at first, as in `git diff`.
        await expect(row(27)).toBeVisible();
        await expect(row(33)).toBeVisible();
        await expect(row(26)).toHaveCount(0);
        await expect(folds).toHaveCount(2);
        await expect(folds.first()).toHaveText(/26 unchanged lines hidden/);

        const requestsAfterLoad = diffRequests;
        const topFold = folds.first();

        // The fold at the top of the file can only grow up from the change below it.
        await expect(
            topFold.getByRole("button", { name: "Expand down" }),
        ).toHaveCount(0);

        await topFold.getByRole("button", { name: "Expand up" }).click();

        // One step reveals the twenty lines just above the visible context.
        await expect(row(7)).toBeVisible();
        await expect(row(6)).toHaveCount(0);
        await expect(topFold).toHaveText(/6 unchanged lines hidden/);

        await topFold.getByRole("button", { name: "Expand all" }).click();

        // Revealing the rest removes the fold, so the file reads from its first line.
        await expect(row(1)).toBeVisible();
        await expect(folds).toHaveCount(1);

        const wholeFile = diff.getByRole("button", {
            name: "Show whole file notes.txt",
        });

        await wholeFile.click();

        // The whole file shows every line with the edit marked in place.
        await expect(wholeFile).toHaveAttribute("aria-pressed", "true");
        await expect(row(60)).toBeVisible();
        await expect(folds).toHaveCount(0);

        await wholeFile.click();

        // Folding again keeps what was revealed by hand and folds only the rest.
        await expect(row(60)).toHaveCount(0);
        await expect(row(1)).toBeVisible();
        await expect(folds).toHaveCount(1);

        // Every line came with the first patch, so expanding never asked the server again.
        expect(diffRequests).toBe(requestsAfterLoad);
    });
});
