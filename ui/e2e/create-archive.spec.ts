import fs from "node:fs/promises";
import path from "node:path";
import { expect, test } from "@playwright/test";
import { $ } from "zx";
import {
    setupTestDir,
    teardownTestDir,
    WEB_BASE_URL,
    type TestContext,
} from "./helpers";

/** Lists archive members so assertions check what was stored, not only that a file appeared. */
async function listTarGzEntries(archivePath: string): Promise<string[]> {
    const result = await $`tar -tzf ${archivePath}`;

    return result.stdout.split("\n").filter((line) => line !== "");
}

/** Resolves once the archive exists, since the terminal writes it asynchronously. */
async function waitForFile(filePath: string): Promise<void> {
    await expect
        .poll(() =>
            fs
                .stat(filePath)
                .then(() => true)
                .catch(() => false),
        )
        .toBe(true);
}

test.describe.serial("Create archive", () => {
    let ctx: TestContext;
    let directoryUrl: string;

    test.beforeAll(async () => {
        ctx = await setupTestDir("create-archive");
        directoryUrl = `${WEB_BASE_URL}/agents/${ctx.agentId}/browser/${ctx.testDirUrlPath}`;
    });

    test.afterAll(async () => {
        await teardownTestDir(ctx.testDirPath);
    });

    test("archives one row from its action menu", async ({ page }) => {
        await page.goto(directoryUrl);
        await page
            .getByRole("button", {
                name: "Actions for file file1.txt",
                exact: true,
            })
            .click();
        await page.getByRole("button", { name: "Create archive" }).click();

        const dialog = page.getByRole("dialog", { name: "Create archive" });

        // A single item suggests its own name so the archive is easy to recognize.
        await expect(
            dialog.getByRole("textbox", { name: "Archive name" }),
        ).toHaveValue("file1.txt");
        // zstd is listed first and is the default format.
        await expect(
            dialog.getByRole("radio", { name: /\.tar\.zst/ }),
        ).toBeChecked();
        // Every offered format is explained in the dialog.
        await expect(dialog.getByRole("radio")).toHaveCount(4);

        await dialog.getByRole("radio", { name: /\.tar\.gz/ }).check();
        // The resulting file name follows the selected format.
        await expect(dialog).toContainText("Saved as file1.txt.tar.gz");
        await dialog
            .getByRole("button", { name: "Create archive", exact: true })
            .click();

        const archivePath = path.join(ctx.testDirPath, "file1.txt.tar.gz");
        await waitForFile(archivePath);
        // The archive stores the file under its own name rather than its absolute path.
        await expect
            .poll(() => listTarGzEntries(archivePath))
            .toEqual(["./file1.txt"]);
        // The listing refreshes after the terminal command so the new archive is visible.
        await expect(
            page.getByRole("link", { name: "file1.txt.tar.gz", exact: true }),
        ).toBeVisible();
    });

    test("archives selected items into the viewed directory", async ({
        page,
    }) => {
        await page.goto(`${directoryUrl}/subdir1`);
        await page
            .getByRole("checkbox", { name: "Select file nested1.txt" })
            .click();
        await page.goto(directoryUrl);
        await page
            .getByRole("checkbox", {
                name: "Select file file2.txt",
                exact: true,
            })
            .click();

        // Delete is icon-only now but keeps its accessible name.
        await expect(
            page.getByRole("button", { name: "Delete selected items" }),
        ).toBeVisible();

        await page
            .getByRole("button", { name: "Create archive of selected items" })
            .click();

        const dialog = page.getByRole("dialog", { name: "Create archive" });

        // Several items have no shared name, so a generic one is suggested.
        await expect(
            dialog.getByRole("textbox", { name: "Archive name" }),
        ).toHaveValue("archive");
        await dialog.getByRole("radio", { name: /\.tar\.gz/ }).check();
        await dialog
            .getByRole("button", { name: "Create archive", exact: true })
            .click();

        const archivePath = path.join(ctx.testDirPath, "archive.tar.gz");
        await waitForFile(archivePath);
        // Items from different directories are stored side by side under their own names.
        await expect
            .poll(async () => (await listTarGzEntries(archivePath)).sort())
            .toEqual(["./file2.txt", "./nested1.txt"]);
        await expect(
            page.getByRole("link", { name: "archive.tar.gz", exact: true }),
        ).toBeVisible();

        await page
            .getByRole("button", { name: "Create archive of selected items" })
            .click();
        await dialog.getByRole("radio", { name: /\.tar\.gz/ }).check();
        await dialog
            .getByRole("button", { name: "Create archive", exact: true })
            .click();
        // An existing entry is refused because tar would overwrite it silently.
        await expect(dialog.getByRole("alert")).toHaveText(
            "archive.tar.gz already exists in this directory",
        );
    });

    test("disables archiving a selection from multiple devices", async ({
        page,
    }) => {
        await page.goto(
            `${WEB_BASE_URL}/agents/${ctx.agent2Id}/browser/${ctx.testDirUrlPath}`,
        );
        await page
            .getByRole("checkbox", {
                name: "Select file file1.txt",
                exact: true,
            })
            .click();
        await page.goto(directoryUrl);
        await page
            .getByRole("checkbox", {
                name: "Select file file2.txt",
                exact: true,
            })
            .click();

        const archiveButton = page.getByRole("button", {
            name: "Create archive of selected items",
        });

        // One terminal cannot read files from two devices.
        await expect(archiveButton).toBeDisabled();
        await archiveButton.hover();
        // The tooltip explains why the action is unavailable.
        await expect(page.getByRole("tooltip")).toHaveText(
            "Archives cannot be created when items are selected from multiple devices",
        );
    });
});
