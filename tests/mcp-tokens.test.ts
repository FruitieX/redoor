import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
    afterAll,
    beforeAll,
    describe,
    expect,
    it,
    onTestFinished,
} from "vitest";

import { ApiClient, type ApiError } from "#ui/api-client";
import { createMcpTestClient } from "./mcp-client";
import {
    ProcessManager,
    TEST_AGENT_TOKEN,
    TEST_PASSWORD,
    TEST_USERNAME,
    VITEST_SERVER_PORT,
    waitForPort,
} from "./test-utils";

const processManager = new ProcessManager();
const baseUrl = `http://127.0.0.1:${VITEST_SERVER_PORT}`;
const endpoint = `${baseUrl}/mcp`;
const temporaryDirectory = mkdtempSync(join(tmpdir(), "redoor-mcp-tokens-"));
const configPath = join(temporaryDirectory, "config.toml");
const deviceId = "managed-token-test-device";

beforeAll(async () => {
    writeFileSync(
        configPath,
        `agent_token = "${TEST_AGENT_TOKEN}"

[server]
username = "${TEST_USERNAME}"
password = "${TEST_PASSWORD}"

[[agents]]
local = true
name = "${deviceId}"
home = "${temporaryDirectory}"
log = "${join(temporaryDirectory, "agent.log")}"
`,
    );
    processManager.spawnServer({ config: configPath });
    await waitForPort(VITEST_SERVER_PORT);
});

afterAll(async () => {
    await processManager.killAll();
    rmSync(temporaryDirectory, { recursive: true, force: true });
});

describe("MCP token management REST API", () => {
    it("enforces browser ownership and performs one-time create, rotation, and revocation", async () => {
        const unauthenticated = await fetch(`${baseUrl}/api/v1/mcp/tokens`);
        // Token administration is never part of the public-path exception.
        expect(unauthenticated.status).toBe(401);
        const bearerOnly = await fetch(`${baseUrl}/api/v1/mcp/tokens`, {
            headers: { Authorization: "Bearer redoor_mcp_invalid_secret" },
        });
        // An MCP bearer cannot bootstrap or escalate other credentials.
        expect(bearerOnly.status).toBe(401);

        const api = new ApiClient(baseUrl);
        await api.login(TEST_USERNAME, TEST_PASSWORD);
        const created = await api.createMcpToken({
            name: "browser managed client",
            scopes: ["devices:read", "files:read"],
            device_ids: [deviceId],
            expires_at: new Date(Date.now() + 86_400_000).toISOString(),
        });
        onTestFinished(async () => {
            try {
                await api.revokeMcpToken(created.credential.credential_id);
            } catch {
                // Rotation intentionally makes the original credential inactive.
            }
        });
        expect(created.token).toMatch(/^redoor_mcp_/);
        const listed = await api.listMcpTokens();
        const metadata = listed.tokens.find(
            (token) => token.credential_id === created.credential.credential_id,
        );
        // Listings expose policy and stable identity but never a secret or verifier.
        expect(metadata).toMatchObject({
            grant_id: created.credential.grant_id,
            scopes: ["devices:read", "files:read"],
            device_ids: [deviceId],
        });
        expect(JSON.stringify(listed)).not.toContain(created.token);
        expect(JSON.stringify(listed)).not.toContain("verifier");

        const client = createMcpTestClient({ endpoint, token: created.token });
        expect(
            (await client.callTool("list_devices")).result.structuredContent
                ?.devices,
        ).toEqual([expect.objectContaining({ device_id: deviceId })]);
        await expect(
            client.callTool("get_device", { device_id: "another-device" }),
        ).rejects.toThrow("(403)");

        const rotated = await api.rotateMcpToken(
            created.credential.credential_id,
        );
        // Stable grant ownership survives the atomic credential replacement.
        expect(rotated.credential.grant_id).toBe(created.credential.grant_id);
        expect(rotated.credential.credential_id).not.toBe(
            created.credential.credential_id,
        );
        await expect(client.callTool("hello", { name: "old" })).rejects.toThrow(
            "(401)",
        );
        const replacement = createMcpTestClient({
            endpoint,
            token: rotated.token,
        });
        expect(
            (await replacement.callTool("hello", { name: "new" })).result
                .structuredContent,
        ).toMatchObject({ greeting: "Hello, new, from Redoor!" });
        await api.revokeMcpToken(rotated.credential.credential_id);
        await expect(
            replacement.callTool("hello", { name: "revoked" }),
        ).rejects.toThrow("(401)");
    });

    it("updates policy in place and enforces changes on the same bearer token", async () => {
        const api = new ApiClient(baseUrl);
        await api.login(TEST_USERNAME, TEST_PASSWORD);
        const created = await api.createMcpToken({
            name: "editable client",
            scopes: ["devices:read"],
            device_ids: [deviceId],
            expires_at: null,
        });
        onTestFinished(async () => {
            await api.revokeMcpToken(created.credential.credential_id);
        });
        const client = createMcpTestClient({ endpoint, token: created.token });
        // The original policy grants inventory access before it is edited.
        expect((await client.callTool("list_devices")).result.structuredContent?.devices)
            .toEqual([expect.objectContaining({ device_id: deviceId })]);
        const id = created.credential.credential_id;
        const saved = await api.updateMcpToken(id, {
            name: "renamed client",
            scopes: [],
            device_ids: [],
        });
        // Editing preserves credential identity and lifecycle, and never exposes a secret.
        expect(saved.credential).toMatchObject({
            credential_id: id,
            grant_id: created.credential.grant_id,
            name: "renamed client",
            scopes: [],
            device_ids: [],
            created_at: created.credential.created_at,
            expires_at: null,
            revoked_at: null,
        });
        expect(JSON.stringify(saved)).not.toContain(created.token);
        expect(JSON.stringify(saved)).not.toContain("verifier");
        // Saved metadata survives a separate read of the durable store.
        expect((await api.listMcpTokens()).tokens.find(token => token.credential_id === id))
            .toMatchObject({ name: "renamed client", scopes: [], device_ids: [] });
        // Permission removal takes effect on the next request with the existing secret.
        await expect(client.callTool("list_devices")).rejects.toThrow("(403)");
        await api.updateMcpToken(id, { name: "renamed client", scopes: ["devices:read"], device_ids: [] });
        // Scope restoration alone cannot bypass the explicitly empty device allowlist.
        expect((await client.callTool("list_devices")).result.structuredContent?.devices).toEqual([]);
        await api.updateMcpToken(id, { name: "renamed client", scopes: ["devices:read"], device_ids: [deviceId] });
        // Device access can be restored without rotating or reconnecting the client.
        expect((await client.callTool("list_devices")).result.structuredContent?.devices)
            .toEqual([expect.objectContaining({ device_id: deviceId })]);
        for (const policy of [
            { name: " ", scopes: [], device_ids: [] },
            { name: "x".repeat(101), scopes: [], device_ids: [] },
            { name: "invalid device", scopes: [], device_ids: ["not-a-device"] },
        ]) {
            // Invalid drafts must leave the saved policy untouched.
            await expect(api.updateMcpToken(id, policy)).rejects.toMatchObject({ status: 400 });
        }
        // Authentication and origin protections also cover policy edits.
        expect((await fetch(`${baseUrl}/api/v1/mcp/tokens/${id}`, {
            method: "PUT",
            headers: { Authorization: `Bearer ${created.token}`, "Content-Type": "application/json" },
            body: JSON.stringify({ name: "unauthorized", scopes: [], device_ids: [] }),
        })).status).toBe(401);
        expect((await fetch(`${baseUrl}/api/v1/mcp/tokens/${id}`, {
            method: "PUT",
            headers: { ...api.getAuthHeaders(), Origin: "https://attacker.example", "Content-Type": "application/json" },
            body: JSON.stringify({ name: "csrf", scopes: [], device_ids: [] }),
        })).status).toBe(403);
        // Missing and malformed identifiers provide predictable API failures.
        await expect(api.updateMcpToken("not-a-uuid", { name: "valid", scopes: [], device_ids: [] }))
            .rejects.toMatchObject({ status: 400 });
        await expect(api.updateMcpToken("00000000-0000-4000-8000-000000000000", { name: "valid", scopes: [], device_ids: [] }))
            .rejects.toMatchObject({ status: 404 });
        const rotated = await api.rotateMcpToken(id);
        onTestFinished(async () => {
            await api.revokeMcpToken(rotated.credential.credential_id);
        });
        // Rotation inherits the edited name and policy.
        expect(rotated.credential).toMatchObject({ name: "renamed client", scopes: ["devices:read"], device_ids: [deviceId] });
        // Historical credentials cannot change a grant used by their active replacement.
        await expect(api.updateMcpToken(id, { name: "historical", scopes: [], device_ids: [] }))
            .rejects.toMatchObject({ status: 409 });
        await api.revokeMcpToken(rotated.credential.credential_id);
        // Revoked credentials remain inactive even if an edit is attempted.
        await expect(api.updateMcpToken(rotated.credential.credential_id, { name: "revoked", scopes: [], device_ids: [] }))
            .rejects.toMatchObject({ status: 409 });
    });

    it("rejects invalid grants, expiry, identifiers, and cross-site mutations", async () => {
        const api = new ApiClient(baseUrl);
        await api.login(TEST_USERNAME, TEST_PASSWORD);
        await expect(
            api.createMcpToken({
                name: "unknown device",
                scopes: ["files:read"],
                device_ids: ["not-a-device"],
                expires_at: null,
            }),
        ).rejects.toMatchObject({ status: 400 } satisfies Partial<ApiError>);
        await expect(
            api.createMcpToken({
                name: "expired",
                scopes: [],
                device_ids: [],
                expires_at: new Date(Date.now() - 60_000).toISOString(),
            }),
        ).rejects.toMatchObject({ status: 400 } satisfies Partial<ApiError>);
        await expect(api.rotateMcpToken("not-a-uuid")).rejects.toMatchObject({
            status: 400,
        } satisfies Partial<ApiError>);

        const crossSite = await fetch(`${baseUrl}/api/v1/mcp/tokens`, {
            method: "POST",
            headers: {
                Cookie: api.getAuthHeaders().Cookie ?? "",
                "Content-Type": "application/json",
                Origin: "https://attacker.example",
            },
            body: JSON.stringify({
                name: "csrf",
                scopes: [],
                device_ids: [],
                expires_at: null,
            }),
        });
        // Browser mutations bind a presented Origin to the actual server host.
        expect(crossSite.status).toBe(403);
    });
});
