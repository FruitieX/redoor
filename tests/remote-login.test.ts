import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { $ } from "zx";
import * as fs from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";

import { ApiClient } from "#ui/api-client";
import { ApiError } from "#ui/api-transport";
import type { AccessTokenResponse } from "#bindings/AccessTokenResponse";
import { createMcpTestClient } from "./mcp-client";
import {
    ProcessManager,
    TEST_APP_NAME,
    TEST_PASSWORD,
    TEST_SERVER_HOME,
    TEST_USERNAME,
    TempFileManager,
    startServerAndAgent,
} from "./test-utils";

describe("Remote CLI login with access tokens", () => {
    const processes = new ProcessManager();
    const files = new TempFileManager();

    const binary =
        process.env.REDOOR_TEST_BINARY ?? path.resolve("target/debug/redoor");

    const sessionsDirectory = path.join(
        TEST_SERVER_HOME,
        ".local/share",
        TEST_APP_NAME,
        "sessions",
    );

    let setup: Awaited<ReturnType<typeof startServerAndAgent>>;
    let home: string;

    beforeAll(async () => {
        setup = await startServerAndAgent({
            processManager: processes,
            agentName: "remote-login-agent",
            agentCwd: files.tempDirectory(),
        });
        home = files.tempDirectory();
    }, 30000);

    afterAll(async () => {
        await processes.killAll();
        files.cleanup();
    });

    /** Runs the real binary in an isolated namespace; empty overrides disable inherited ones. */
    function remote(
        namespace: string,
        args: string[],
        options: { input?: string; env?: Record<string, string> } = {},
    ) {
        return $({
            env: {
                ...process.env,
                HOME: home,
                REDOOR_APP_NAME: namespace,
                REDOOR_REMOTE_SERVER: "",
                REDOOR_REMOTE_TOKEN: "",
                ...options.env,
            },
            input: options.input,
            quiet: true,
            nothrow: true,
        })`exec ${binary} remote ${args}`;
    }

    /** Logs in non-interactively with a fixed policy, as a script would. */
    function login(namespace: string, permissions: string) {
        return remote(
            namespace,
            [
                "login",
                setup.apiClient.baseUrl,
                "--username",
                TEST_USERNAME,
                "--password-stdin",
                "--permissions",
                permissions,
                "--devices",
                "all",
                "--name",
                `login test ${namespace}`,
                "--expires",
                "never",
                "--yes",
            ],
            { input: `${TEST_PASSWORD}\n` },
        );
    }

    /** Path of a namespace's saved credentials. */
    function credentialsPath(namespace: string) {
        return path.join(home, ".local/share", namespace, "remote-credentials.json");
    }

    /** Reads the token a namespace saved. */
    async function savedToken(namespace: string): Promise<string> {
        const saved: { token: string } = JSON.parse(
            await fs.readFile(credentialsPath(namespace), "utf8"),
        );

        return saved.token;
    }

    /** Resolves to the HTTP status of a token's self lookup. */
    async function tokenStatus(token: string): Promise<number> {
        const client = new ApiClient(setup.apiClient.baseUrl);
        client.setAccessToken(token);

        try {
            await client.getCurrentAccessToken();

            return 200;
        } catch (error) {
            if (error instanceof ApiError) return error.status;

            throw error;
        }
    }

    it("logs in with flags, saves only a token and logs the transient session out", async () => {
        const sessionsBefore = (await fs.readdir(sessionsDirectory)).length;
        const result = await login("login-flags", "files:read,agents:manage");

        // A scripted login succeeds and reports the granted policy on stderr.
        expect(result.exitCode, result.stderr).toBe(0);
        expect(result.stderr).toContain("Logged in to");
        expect(result.stderr).toContain("files:read, agents:manage");

        const stat = await fs.stat(credentialsPath("login-flags"));
        const contents = await fs.readFile(credentialsPath("login-flags"), "utf8");
        // The file is private and holds a token, never the password.
        expect(stat.mode & 0o777).toBe(0o600);
        expect(contents).toContain("redoor_at_");
        expect(contents).not.toContain(TEST_PASSWORD);

        const status = await remote("login-flags", ["status", "--json"]);
        expect(status.exitCode, status.stderr).toBe(0);
        const current: AccessTokenResponse = JSON.parse(status.stdout);
        // Status reports exactly the permissions chosen at login.
        expect(current.permissions).toEqual(["files:read", "agents:manage"]);
        expect(current.devices).toEqual({ kind: "all" });
        // The server lists the new token.
        expect(
            (await setup.apiClient.listAccessTokens()).tokens.map(
                (token) => token.id,
            ),
        ).toContain(current.id);
        // The password session used to create the token was logged out, so no session remains.
        expect((await fs.readdir(sessionsDirectory)).length).toBe(sessionsBefore);

        const exec = await remote("login-flags", [
            "exec",
            setup.testAgent.id,
            "--",
            "true",
        ]);

        // The token lacks commands:execute, and the error says so.
        expect(exec.exitCode).not.toBe(0);
        expect(exec.stderr).toContain("commands:execute");
    });

    it("rejects a wrong password without saving anything", async () => {
        const result = await remote(
            "login-wrong",
            [
                "login",
                setup.apiClient.baseUrl,
                "--username",
                TEST_USERNAME,
                "--password-stdin",
                "--permissions",
                "files:read",
                "--yes",
            ],
            { input: "not the password\n" },
        );

        // A rejected password fails the command.
        expect(result.exitCode).not.toBe(0);
        expect(result.stderr).toContain("invalid username or password");
        // Nothing is written on failure.
        expect(existsSync(credentialsPath("login-wrong"))).toBe(false);
    });

    it("names missing flags when not interactive", async () => {
        const result = await remote(
            "login-missing",
            ["login", setup.apiClient.baseUrl, "--username", TEST_USERNAME, "--password-stdin"],
            { input: `${TEST_PASSWORD}\n` },
        );

        // Scripts learn which flag to add instead of hanging on a prompt.
        expect(result.exitCode).not.toBe(0);
        expect(result.stderr).toContain("--permissions");
    });

    it("revokes the previous token on re-login and on logout", async () => {
        expect((await login("login-again", "files:read")).exitCode).toBe(0);
        const first = await savedToken("login-again");

        const again = await login("login-again", "files:read,files:write");
        expect(again.exitCode, again.stderr).toBe(0);
        const second = await savedToken("login-again");

        // Logging in again replaces the token and revokes the old one, leaving no orphan.
        expect(second).not.toBe(first);
        expect(await tokenStatus(first)).toBe(401);
        expect(await tokenStatus(second)).toBe(200);

        const logout = await remote("login-again", ["logout"]);
        // Logout revokes the token server-side and removes the file.
        expect(logout.exitCode, logout.stderr).toBe(0);
        expect(await tokenStatus(second)).toBe(401);
        expect(existsSync(credentialsPath("login-again"))).toBe(false);
    });

    it("creates a token for MCP clients without saving it", async () => {
        const result = await remote(
            "create-token",
            [
                "create-token",
                setup.apiClient.baseUrl,
                "--username",
                TEST_USERNAME,
                "--password-stdin",
                "--permissions",
                "files:read",
                "--yes",
            ],
            { input: `${TEST_PASSWORD}\n` },
        );

        expect(result.exitCode, result.stderr).toBe(0);

        const output: {
            token: string;
            mcp_endpoint: string;
            metadata: AccessTokenResponse;
        } = JSON.parse(result.stdout);

        // The JSON carries the secret and where to use it.
        expect(output.token).toMatch(/^redoor_at_/);
        expect(output.mcp_endpoint).toBe(`${setup.apiClient.baseUrl}/mcp`);
        expect(output.metadata.permissions).toEqual(["files:read"]);
        // Nothing is saved locally.
        expect(existsSync(credentialsPath("create-token"))).toBe(false);

        const tools = await createMcpTestClient({
            endpoint: output.mcp_endpoint,
            token: output.token,
        }).listTools();

        // The printed token works on the MCP endpoint, limited to its permissions.
        expect(tools.result.tools.map((tool) => tool.name)).toContain("read_file");
        expect(tools.result.tools.map((tool) => tool.name)).not.toContain(
            "run_command",
        );
        await setup.apiClient.deleteAccessToken(output.metadata.id);
    });

    it("uses the environment override without a saved login", async () => {
        const created = await setup.apiClient.createAccessToken({
            name: "env override",
            permissions: ["files:read"],
            devices: { kind: "all" },
            expires_at: null,
        });

        const devices = await remote("env-override", ["devices", "--json"], {
            env: {
                REDOOR_REMOTE_SERVER: setup.apiClient.baseUrl,
                REDOOR_REMOTE_TOKEN: created.token,
            },
        });

        // Both variables authenticate without any credentials file.
        expect(devices.exitCode, devices.stderr).toBe(0);
        expect(JSON.parse(devices.stdout).agents).toEqual(
            expect.arrayContaining([
                expect.objectContaining({ id: setup.testAgent.id }),
            ]),
        );
        expect(existsSync(credentialsPath("env-override"))).toBe(false);

        const half = await remote("env-override", ["devices"], {
            env: { REDOOR_REMOTE_TOKEN: created.token },
        });

        // Only one of the variables is a configuration mistake, not a silent fallback.
        expect(half.exitCode).not.toBe(0);
        expect(half.stderr).toContain("REDOOR_REMOTE_SERVER");
        await setup.apiClient.deleteAccessToken(created.metadata.id);
    });
});
