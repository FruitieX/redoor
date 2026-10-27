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

/** The worktree files the tests edit, so each test sees the same mixed change set. */
const files = ["src/server.ts", "src/server.test.ts", "src/my-test/helpers.ts"];

test.describe("Git diff list", () => {
    let testDirPath = "";
    let repositoryPath = "";
    let agentName = "";

    test.beforeEach(async () => {
        const ctx = await setupTestDir("git-diff-list");
        testDirPath = ctx.testDirPath;
        agentName = ctx.agentName;
        repositoryPath = path.join(ctx.testDirPath, "repository");
        await fs.mkdir(path.join(repositoryPath, "src", "my-test"), {
            recursive: true,
        });

        for (const file of files) {
            await fs.writeFile(
                path.join(repositoryPath, file),
                'export const mode = "committed";\n',
            );
        }

        await $`git -C ${repositoryPath} init`;
        await $`git -C ${repositoryPath} config user.email playwright@redoor.test`;
        await $`git -C ${repositoryPath} config user.name Playwright`;
        await $`git -C ${repositoryPath} add .`;
        await $`git -C ${repositoryPath} commit -m baseline`;

        for (const file of files) {
            await fs.writeFile(
                path.join(repositoryPath, file),
                'export const mode = "edited";\n',
            );
        }
    });

    test.afterEach(async () => {
        await teardownTestDir(testDirPath);
    });

    test("switches the diff layout of one file without changing the others", async ({
        page,
    }) => {
        await page.goto(
            `${WEB_BASE_URL}/agents/${agentName}/browser/${encodeFilesystemPath(repositoryPath)}?view=git`,
        );

        await page.getByRole("button", { name: "Expand all" }).click();

        const server = page.getByRole("region", {
            name: `Git diff for ${path.join(repositoryPath, "src/server.ts")}`,
        });

        const serverTest = page.getByRole("region", {
            name: `Git diff for ${path.join(repositoryPath, "src/server.test.ts")}`,
        });

        // Each file carries its own layout switch, starting from the stored inline default.
        await expect(
            serverTest
                .getByRole("group", {
                    name: "Diff layout for src/server.test.ts",
                })
                .getByRole("button", { name: "Inline changes" }),
        ).toHaveAttribute("aria-pressed", "true");
        await expect(
            serverTest.locator('tr[data-line-kind="modified"]'),
        ).toHaveCount(1);

        const serverLayout = server.getByRole("group", {
            name: "Diff layout for src/server.ts",
        });

        await serverLayout
            .getByRole("button", { name: "Separate lines" })
            .click();

        // The switched file lists its removed and added lines as their own rows.
        await expect(
            serverLayout.getByRole("button", { name: "Separate lines" }),
        ).toHaveAttribute("aria-pressed", "true");
        await expect(server.locator('tr[data-line-kind="delete"]')).toHaveCount(
            1,
        );
        await expect(
            server.locator('tr[data-line-kind="modified"]'),
        ).toHaveCount(0);

        // The other file keeps its inline row, so the choice was only for one file.
        await expect(
            serverTest.locator('tr[data-line-kind="modified"]'),
        ).toHaveCount(1);
        await expect(
            serverTest.locator('tr[data-line-kind="delete"]'),
        ).toHaveCount(0);
    });

    test("filters files by name and excludes dashed words", async ({
        page,
    }) => {
        await page.goto(
            `${WEB_BASE_URL}/agents/${agentName}/browser/${encodeFilesystemPath(repositoryPath)}?view=git`,
        );

        const headers = page.getByRole("button", { name: /^Expand diff for/ });

        const filter = page.getByRole("textbox", {
            name: "Filter files by name",
        });

        // Without a filter every changed file is listed.
        await expect(headers).toHaveCount(3);

        await filter.fill("-test");

        // A dashed word hides every path containing it, test files and directories alike.
        await expect(headers).toHaveCount(1);
        await expect(
            page.getByRole("button", { name: "Expand diff for src/server.ts" }),
        ).toBeVisible();
        // The count tells that files are hidden rather than unchanged.
        await expect(page.getByText("Showing 1 of 3 files")).toBeVisible();

        await filter.fill('"-test"');

        // Quotes make the dash part of the word, so only the dashed directory matches.
        await expect(headers).toHaveCount(1);
        await expect(
            page.getByRole("button", {
                name: "Expand diff for src/my-test/helpers.ts",
            }),
        ).toBeVisible();

        await filter.fill("server -nothing");

        // Plain words must all appear, and an exclusion that matches nothing hides nothing.
        await expect(headers).toHaveCount(2);

        await filter.fill("missing");

        // An unmatched filter says so instead of looking like a clean worktree.
        await expect(headers).toHaveCount(0);
        await expect(page.getByText("No files match the filter")).toBeVisible();

        await page.getByRole("button", { name: "Clear file filter" }).click();

        // Clearing the filter lists every file again.
        await expect(headers).toHaveCount(3);
    });
});
