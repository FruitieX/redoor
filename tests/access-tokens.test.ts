import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { ApiClient, type Agent } from "#ui/api-client";
import { ApiError } from "#ui/api-transport";
import {
    ProcessManager,
    TempFileManager,
    VITEST_SERVER_PORT,
    createAccessToken,
    startServerAndAgent,
    waitForPort,
    waitForValue,
} from "./test-utils";

const processManager = new ProcessManager();

const tempFiles = new TempFileManager();

const baseUrl = `http://127.0.0.1:${VITEST_SERVER_PORT}`;

let api: ApiClient;

let agent: Agent;

let serverPid: number;

/** Builds a client that authenticates only with the given access token. */
function asToken(token: string): ApiClient {
    const client = new ApiClient(baseUrl);
    client.setAccessToken(token);

    return client;
}

/** Resolves to the HTTP status of an API call, so assertions can compare statuses directly. */
async function statusOf(call: Promise<unknown>): Promise<number> {
    try {
        await call;

        return 200;
    } catch (error) {
        if (error instanceof ApiError) {
            return error.status;
        }

        throw error;
    }
}

/** Starts a trivial command and reports the status, consuming the stream to completion. */
async function execStatus(token: string): Promise<number> {
    const response = await fetch(
        `${baseUrl}/api/v1/agents/${encodeURIComponent(agent.id)}/exec`,
        {
            method: "POST",
            headers: {
                Authorization: `Bearer ${token}`,
                "Content-Type": "application/json",
            },
            body: JSON.stringify({ argv: ["true"], cwd: null, env: {} }),
        },
    );

    await response.arrayBuffer();

    return response.status;
}

beforeAll(async () => {
    const setup = await startServerAndAgent({
        processManager,
        agentName: "access-token-agent",
        agentCwd: tempFiles.tempDirectory(),
    });

    api = setup.apiClient;
    agent = setup.testAgent;
    serverPid = setup.serverPid;
}, 30_000);

afterAll(async () => {
    await processManager.killAll();
    tempFiles.cleanup();
});

describe("Access token REST API", () => {
    it("returns the secret once and never lists it", async () => {
        const created = await createAccessToken(api, {
            name: "listing test",
            permissions: ["files:read"],
        });

        // The prefix makes leaked tokens recognizable to secret scanners.
        expect(created.token).toMatch(/^redoor_at_[0-9a-f]{32}_/);
        // The metadata reflects the requested policy.
        expect(created.metadata).toMatchObject({
            name: "listing test",
            permissions: ["files:read"],
            devices: { kind: "all" },
            expires_at: null,
            last_used_at: null,
        });

        const response = await fetch(`${baseUrl}/api/v1/tokens`, {
            headers: api.getAuthHeaders(),
        });

        const body = await response.text();
        // Token responses must not be cached by browsers or proxies.
        expect(response.headers.get("cache-control")).toContain("no-store");
        // Listings contain the token but never its secret or verifier.
        expect(body).toContain(created.metadata.id);
        expect(body).not.toContain(created.token);
        expect(body).not.toContain("verifier");
    });

    it("rejects invalid policies", async () => {
        const invalid = [
            {
                name: " ",
                permissions: ["files:read" as const],
                devices: { kind: "all" as const },
                expires_at: null,
            },
            {
                name: "no permissions",
                permissions: [],
                devices: { kind: "all" as const },
                expires_at: null,
            },
            {
                name: "empty scope",
                permissions: ["files:read" as const],
                devices: { kind: "only" as const, device_ids: [] },
                expires_at: null,
            },
            {
                name: "unknown device",
                permissions: ["files:read" as const],
                devices: {
                    kind: "only" as const,
                    device_ids: ["no-such-device"],
                },
                expires_at: null,
            },
            {
                name: "past expiry",
                permissions: ["files:read" as const],
                devices: { kind: "all" as const },
                expires_at: new Date(Date.now() - 60_000).toISOString(),
            },
        ];

        for (const request of invalid) {
            // Each invalid policy is refused instead of creating an unusable token.
            expect(await statusOf(api.createAccessToken(request))).toBe(400);
        }

        const unknownPermission = await fetch(`${baseUrl}/api/v1/tokens`, {
            method: "POST",
            headers: {
                ...api.getAuthHeaders(),
                "Content-Type": "application/json",
            },
            body: JSON.stringify({
                name: "unknown permission",
                permissions: ["devices:read"],
                devices: { kind: "all" },
                expires_at: null,
            }),
        });

        // Unknown permission names fail request decoding rather than granting nothing.
        expect(unknownPermission.status).toBe(422);
        // None of the rejected requests left a token behind.
        expect(
            (await api.listAccessTokens()).tokens.some((token) =>
                [
                    "no permissions",
                    "empty scope",
                    "unknown device",
                    "past expiry",
                    "unknown permission",
                ].includes(token.name),
            ),
        ).toBe(false);
    });

    it("applies updated permissions to the same bearer at once", async () => {
        const created = await createAccessToken(api, {
            name: "editable",
            permissions: ["files:read", "commands:execute"],
        });

        // The original policy allows execution.
        expect(await execStatus(created.token)).toBe(200);

        const updated = await api.updateAccessToken(created.metadata.id, {
            name: "edited",
            permissions: ["files:read"],
            devices: { kind: "only", device_ids: [agent.id] },
        });

        // The update keeps the token's identity and returns the new policy.
        expect(updated).toMatchObject({
            id: created.metadata.id,
            name: "edited",
            permissions: ["files:read"],
            devices: { kind: "only", device_ids: [agent.id] },
        });
        // The next request with the unchanged secret already lacks the removed permission.
        expect(await execStatus(created.token)).toBe(403);
        // Permissions that remain still work.
        expect(
            await statusOf(asToken(created.token).getCurrentAccessToken()),
        ).toBe(200);
    });

    it("rotates the secret but keeps the id", async () => {
        const created = await createAccessToken(api, {
            name: "rotating",
            permissions: ["files:read"],
        });

        const rotated = await api.rotateAccessToken(created.metadata.id);

        // Rotation keeps the id, so MCP operations owned by the token survive it.
        expect(rotated.metadata.id).toBe(created.metadata.id);
        expect(rotated.token).not.toBe(created.token);
        // The old secret stops working immediately.
        expect(
            await statusOf(asToken(created.token).getCurrentAccessToken()),
        ).toBe(401);
        // The new secret works.
        expect(
            (await asToken(rotated.token).getCurrentAccessToken()).id,
        ).toBe(created.metadata.id);
    });

    it("rejects expired tokens and refuses to revive them", async () => {
        const created = await createAccessToken(api, {
            name: "expiring",
            permissions: ["files:read"],
            expiresAt: new Date(Date.now() + 1_500).toISOString(),
        });

        const client = asToken(created.token);
        // The token works before it expires.
        expect(await statusOf(client.getCurrentAccessToken())).toBe(200);
        // Expiry is checked on every request, so the token is rejected once it passes.
        await waitForValue({
            description: "expired token rejection",
            timeoutMs: 10_000,
            predicate: async () =>
                (await statusOf(client.getCurrentAccessToken())) === 401
                    ? true
                    : undefined,
        });
        // Rotating or editing an expired token would revive a dead credential.
        expect(
            await statusOf(api.rotateAccessToken(created.metadata.id)),
        ).toBe(409);
        expect(
            await statusOf(
                api.updateAccessToken(created.metadata.id, {
                    name: "revived",
                    permissions: ["files:read"],
                    devices: { kind: "all" },
                }),
            ),
        ).toBe(409);
        // Expired tokens stay listed so users can see why a client stopped working.
        expect(
            (await api.listAccessTokens()).tokens.some(
                (token) => token.id === created.metadata.id,
            ),
        ).toBe(true);
    });

    it("revokes on delete", async () => {
        const created = await createAccessToken(api, {
            name: "deleted",
            permissions: ["files:read"],
        });

        await api.deleteAccessToken(created.metadata.id);
        // Deletion takes effect on the very next request.
        expect(
            await statusOf(asToken(created.token).getCurrentAccessToken()),
        ).toBe(401);
        // Unknown and malformed ids are distinct client errors.
        expect(await statusOf(api.deleteAccessToken(created.metadata.id))).toBe(
            404,
        );
        expect(await statusOf(api.deleteAccessToken("not-a-uuid"))).toBe(400);
    });

    it("rejects cross-site token mutations", async () => {
        const body = JSON.stringify({
            name: "csrf",
            permissions: ["files:read"],
            devices: { kind: "all" },
            expires_at: null,
        });

        for (const [name, value] of [
            ["Sec-Fetch-Site", "cross-site"],
            ["Origin", "https://attacker.example"],
        ] as const) {
            const headers = new Headers(api.getAuthHeaders());
            headers.set(name, value);
            headers.set("Content-Type", "application/json");

            const response = await fetch(`${baseUrl}/api/v1/tokens`, {
                method: "POST",
                headers,
                body,
            });

            // Another site cannot use the browser session cookie to mint tokens.
            expect(response.status).toBe(403);
        }
    });

    it("never lets a token manage tokens", async () => {
        const created = await createAccessToken(api, {
            name: "no escalation",
            permissions: [
                "files:read",
                "files:write",
                "files:delete",
                "commands:execute",
                "agents:manage",
            ],
        });

        const client = asToken(created.token);
        // Even a full-permission token cannot list tokens.
        expect(await statusOf(client.listAccessTokens())).toBe(403);
        // Nor mint a broader one.
        expect(
            await statusOf(
                client.createAccessToken({
                    name: "escalated",
                    permissions: ["files:read"],
                    devices: { kind: "all" },
                    expires_at: null,
                }),
            ),
        ).toBe(403);
        // Nor rotate or delete other tokens by id.
        expect(
            await statusOf(client.rotateAccessToken(created.metadata.id)),
        ).toBe(403);
    });

    it("lets a token read and delete itself through /current", async () => {
        const created = await createAccessToken(api, {
            name: "self",
            permissions: ["files:read", "agents:manage"],
        });

        const client = asToken(created.token);
        const current = await client.getCurrentAccessToken();

        // A token learns its own policy, which CLI status relies on.
        expect(current).toMatchObject({
            id: created.metadata.id,
            permissions: ["files:read", "agents:manage"],
        });
        // The request that just authenticated is recorded as a use.
        expect(current.last_used_at).not.toBeNull();
        // A browser session has no "current token".
        expect(await statusOf(api.getCurrentAccessToken())).toBe(400);

        const deleted = await fetch(`${baseUrl}/api/v1/tokens/current`, {
            method: "DELETE",
            headers: client.getAuthHeaders(),
        });

        // Self-deletion is what CLI logout uses.
        expect(deleted.status).toBe(200);
        expect(await statusOf(client.getCurrentAccessToken())).toBe(401);
    });

    it("keeps tokens across a server restart", async () => {
        const created = await createAccessToken(api, {
            name: "durable",
            permissions: ["files:read"],
        });

        await processManager.killAndWait(serverPid);
        serverPid = processManager.spawnServer({});
        await waitForPort(VITEST_SERVER_PORT);

        // The token store is persisted, so existing clients keep working after a restart.
        expect(
            (await asToken(created.token).getCurrentAccessToken()).name,
        ).toBe("durable");
    });
});
