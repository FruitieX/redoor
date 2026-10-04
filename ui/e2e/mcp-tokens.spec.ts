import { expect, test } from "@playwright/test";
import { WEB_BASE_URL } from "./helpers";

test.describe("MCP token settings", () => {
    test("creates, copies, edits, rotates, and revokes a least-privilege token", async ({
        page,
        context,
    }) => {
        let tokenName = `Playwright MCP client ${Date.now()}`;
        await context.grantPermissions(["clipboard-read", "clipboard-write"]);
        await page.goto(`${WEB_BASE_URL}/settings/mcp`);
        await expect(
            page.getByRole("heading", { name: "MCP tokens" }),
        ).toBeVisible();

        await page.getByRole("button", { name: "Create token" }).click();
        const createDialog = page.getByRole("dialog", {
            name: "Create MCP token",
        });
        await expect(
            createDialog.getByRole("checkbox", { name: /Device inventory/ }),
        ).toBeChecked();
        await expect(
            createDialog.getByRole("checkbox", { name: /Read files/ }),
        ).toBeChecked();
        // Shell and every device require an explicit opt-in beyond the read-only defaults.
        await expect(
            createDialog.getByRole("checkbox", { name: /Run shell commands/ }),
        ).not.toBeChecked();
        const deviceCheckboxes = createDialog
            .getByRole("group", { name: "Allowed devices" })
            .getByRole("checkbox");
        await expect(deviceCheckboxes.first()).not.toBeChecked();
        await createDialog.getByLabel("Token name").fill(tokenName);
        await deviceCheckboxes.first().click();
        await createDialog
            .getByRole("button", { name: "Create token" })
            .click();

        const secretDialog = page.getByRole("dialog", {
            name: "Token created",
        });
        const originalToken = await secretDialog
            .getByLabel("Bearer token contents")
            .textContent();
        expect(originalToken).toMatch(/^redoor_mcp_/);
        await secretDialog
            .getByRole("button", { name: "Copy MCP bearer token" })
            .click();
        await expect(
            secretDialog.getByRole("button", { name: "Copied!" }),
        ).toBeVisible();
        await secretDialog
            .getByRole("button", { name: "I saved the token" })
            .click();
        await expect(
            page.getByText(originalToken ?? "missing-token"),
        ).toHaveCount(0);

        const card = page.getByRole("article").filter({
            has: page.getByRole("heading", { name: tokenName, exact: true }),
        });
        await expect(card.getByText("Active")).toBeVisible();
        await card.getByRole("button", { name: "Edit" }).click();
        const editDialog = page.getByRole("dialog", { name: "Edit MCP token" });
        // Editing starts with the saved name, permissions, and device selection.
        await expect(editDialog.getByLabel("Token name")).toHaveValue(
            tokenName,
        );
        await expect(
            editDialog.getByRole("checkbox", { name: /Read files/ }),
        ).toBeChecked();
        await expect(
            editDialog
                .getByRole("group", { name: "Allowed devices" })
                .getByRole("checkbox")
                .first(),
        ).toBeChecked();
        await editDialog.getByLabel("Token name").fill("discarded draft");
        await editDialog
            .getByRole("button", { name: "Cancel", exact: true })
            .click();
        // Cancel discards the draft and reopening uses the saved values.
        await expect(
            card.getByRole("heading", { name: tokenName, exact: true }),
        ).toBeVisible();
        await card.getByRole("button", { name: "Edit" }).click();
        await expect(editDialog.getByLabel("Token name")).toHaveValue(
            tokenName,
        );
        await editDialog.getByLabel("Token name").fill("");
        // A blank name cannot be saved.
        await expect(
            editDialog.getByRole("button", { name: "Save changes" }),
        ).toBeDisabled();
        tokenName += " edited";
        await editDialog.getByLabel("Token name").fill(tokenName);
        await editDialog.getByRole("checkbox", { name: /Read files/ }).click();
        await editDialog.getByRole("checkbox", { name: /Write files/ }).click();
        await editDialog
            .getByRole("group", { name: "Allowed devices" })
            .getByRole("checkbox")
            .first()
            .click();
        await editDialog.getByRole("button", { name: "Save changes" }).click();
        await expect(editDialog).not.toBeVisible();
        await page.reload();
        const editedCard = page.getByRole("article").filter({
            has: page.getByRole("heading", { name: tokenName, exact: true }),
        });
        // Reload confirms that the edited policy is persisted without exposing a new secret.
        await expect(editedCard).toContainText("devices:read, files:write");
        await expect(editedCard).toContainText("Devices: None");
        await expect(page.getByRole("dialog")).toHaveCount(0);
        await editedCard.getByRole("button", { name: "Rotate" }).click();
        await page
            .getByRole("dialog", { name: "Rotate MCP token?" })
            .getByRole("button", { name: "Rotate token" })
            .click();
        const rotatedDialog = page.getByRole("dialog", {
            name: "Token rotated",
        });
        const rotatedToken = await rotatedDialog
            .getByLabel("Bearer token contents")
            .textContent();
        // A rotation must reveal different material only in its immediate workflow.
        expect(rotatedToken).toMatch(/^redoor_mcp_/);
        expect(rotatedToken).not.toBe(originalToken);
        await rotatedDialog
            .getByRole("button", { name: "I saved the token" })
            .click();

        const activeCard = page
            .getByRole("article")
            .filter({
                has: page.getByRole("heading", {
                    name: tokenName,
                    exact: true,
                }),
            })
            .filter({ hasText: "Active" });
        await activeCard.getByRole("button", { name: "Revoke" }).click();
        await page
            .getByRole("dialog", { name: "Revoke MCP token?" })
            .getByRole("button", { name: "Revoke token" })
            .click();
        await expect(page.getByRole("status")).toContainText("Token revoked");
        const revokedCards = page
            .getByRole("article")
            .filter({
                has: page.getByRole("heading", {
                    name: tokenName,
                    exact: true,
                }),
            })
            .filter({ hasText: "Revoked" });
        // Rotation history retains the invalidated predecessor beside the revoked replacement.
        await expect(revokedCards).toHaveCount(2);

        await page.reload();
        // Navigation and list reload restore metadata only, never either one-time secret.
        await expect(
            page.getByText(originalToken ?? "missing-token"),
        ).toHaveCount(0);
        await expect(
            page.getByText(rotatedToken ?? "missing-token"),
        ).toHaveCount(0);
    });
});
