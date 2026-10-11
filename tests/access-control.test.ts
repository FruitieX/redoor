import { mkdirSync, writeFileSync, existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
    Agent,
    ApiClient,
    ApiError,
    type DeviceScope,
    type Permission,
} from "#ui/api-client";
import type { AgentListResponse } from "#bindings/AgentListResponse";
import type { AgentInfoResponse } from "#bindings/AgentInfoResponse";
import {
    ALL_PERMISSIONS,
    ProcessManager,
    TEST_AGENT_TOKEN,
    TEST_PASSWORD,
    TEST_USERNAME,
    TempFileManager,
    VITEST_SERVER_PORT,
    createAccessToken,
    waitForAgentReady,
    waitForPort,
    waitForValue,
} from "./test-utils";

const processManager = new ProcessManager();

const tempFiles = new TempFileManager();

const baseUrl = `http://127.0.0.1:${VITEST_SERVER_PORT}`;

const api = new ApiClient(baseUrl);

const agentA = "access-control-a";

const agentB = "access-control-b";

const managedAgent = "access-control-managed";

let rootA: string;

let rootB: string;

let trashA: string;

const infos = new Map<string, AgentInfoResponse>();

/** Builds an agent handle that authenticates only with the given access token. */
function agentAs(token: string, id: string): Agent {
    const info = infos.get(id);

    if (info === undefined) throw new Error(`Unknown test agent ${id}`);

    return new Agent(baseUrl, info, { getAccessToken: () => token });
}

/** Builds a client that authenticates only with the given access token. */
function clientAs(token: string): ApiClient {
    const client = new ApiClient(baseUrl);
    client.setAccessToken(token);

    return client;
}

/** Mints a token for one test; it is deleted when the test finishes. */
async function token(
    permissions: Permission[],
    devices: DeviceScope = { kind: "all" },
): Promise<string> {
    return (await createAccessToken(api, { permissions, devices })).token;
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

/** Sends a raw request with a bearer token and returns its status, draining the body. */
async function rawStatus(request: {
    bearer: string;
    method: string;
    path: string;
    /** Serialized JSON request body, when the route takes one. */
    json?: string;
}): Promise<number> {
    const headers = new Headers({ Authorization: `Bearer ${request.bearer}` });

    if (request.json !== undefined) {
        headers.set("Content-Type", "application/json");
    }

    const response = await fetch(`${baseUrl}${request.path}`, {
        method: request.method,
        headers,
        body: request.json,
    });

    await response.arrayBuffer();

    return response.status;
}

/** Spawns one standalone agent and waits until its connection is ready. */
async function spawnAgent(options: {
    serverPid: number;
    name: string;
    root: string;
    env?: NodeJS.ProcessEnv;
}): Promise<void> {
    const server = processManager.getProcess(options.serverPid);

    if (server === undefined) throw new Error("Server process missing");
    const ready = waitForAgentReady(server, options.name);
    processManager.spawnAgent({
        wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}/api/v1/agent/ws`,
        name: options.name,
        cwd: options.root,
        home: options.root,
        env: options.env,
    });
    await ready;
}

beforeAll(async () => {
    const directory = tempFiles.tempDirectory();
    rootA = join(directory, "a");
    rootB = join(directory, "b");
    trashA = join(directory, "trash-a");
    const managedRoot = join(directory, "managed");

    for (const path of [rootA, rootB, managedRoot]) mkdirSync(path);
    const configPath = join(directory, "config.toml");
    writeFileSync(
        configPath,
        `agent_token = "${TEST_AGENT_TOKEN}"

[server]
username = "${TEST_USERNAME}"
password = "${TEST_PASSWORD}"

[[agents]]
local = true
name = "${managedAgent}"
home = "${managedRoot}"
log = "${join(directory, "managed.log")}"
`,
    );
    process.env.REDOOR_PORT = VITEST_SERVER_PORT.toString();
    const serverPid = processManager.spawnServer({ config: configPath });
    await waitForPort(VITEST_SERVER_PORT);
    await api.login(TEST_USERNAME, TEST_PASSWORD);
    await spawnAgent({
        serverPid,
        name: agentA,
        root: rootA,
        env: { REDOOR_AGENT_TRASH_DIRECTORY: trashA },
    });
    await spawnAgent({ serverPid, name: agentB, root: rootB });

    const response = await fetch(`${baseUrl}/api/v1/agents`, {
        headers: api.getAuthHeaders(),
    });

    const listed: AgentListResponse = await response.json();

    for (const info of listed.agents) infos.set(info.id, info);
}, 30_000);

afterAll(async () => {
    await processManager.killAll();
    tempFiles.cleanup();
});

describe("Access token permissions", () => {
    it("files:read gates listing, metadata and downloads", async () => {
        const file = join(rootA, "read.txt");
        writeFileSync(file, "readable");
        const without = agentAs(await token(["files:write"]), agentA);
        const withRead = agentAs(await token(["files:read"]), agentA);

        for (const call of [
            (agent: Agent) => agent.ls(rootA),
            (agent: Agent) => agent.metadata(file),
            (agent: Agent) => agent.raw(file),
        ]) {
            // A token without files:read is refused before the agent is contacted.
            expect(await statusOf(call(without))).toBe(403);
            // The same request succeeds with files:read.
            expect(await statusOf(call(withRead))).toBe(200);
        }
    });

    it("files:write gates uploads and directory creation", async () => {
        const without = agentAs(await token(["files:read"]), agentA);
        const withWrite = agentAs(await token(["files:write"]), agentA);

        const upload = (agent: Agent, name: string) =>
            agent.upload(join(rootA, name), new File(["written"], name));

        // Uploads need files:write and must not create the file when refused.
        expect(await statusOf(upload(without, "denied.txt"))).toBe(403);
        expect(existsSync(join(rootA, "denied.txt"))).toBe(false);
        expect(await statusOf(upload(withWrite, "allowed.txt"))).toBe(200);
        expect(readFileSync(join(rootA, "allowed.txt"), "utf8")).toBe(
            "written",
        );
        // Directory creation is a write as well.
        expect(
            await statusOf(without.createDirectory(join(rootA, "denied-dir"))),
        ).toBe(403);
        expect(
            await statusOf(
                withWrite.createDirectory(join(rootA, "allowed-dir")),
            ),
        ).toBe(200);
    });

    it("files:delete gates permanent deletion, trash and emptying the trash", async () => {
        const permanent = join(rootA, "delete-permanent.txt");
        const trashed = join(rootA, "delete-trash.txt");
        writeFileSync(permanent, "x");
        writeFileSync(trashed, "x");

        const without = agentAs(
            await token(["files:read", "files:write"]),
            agentA,
        );

        const withDelete = agentAs(await token(["files:delete"]), agentA);

        // Deleting needs files:delete even with read and write.
        expect(await statusOf(without.deleteFile(permanent))).toBe(403);
        expect(
            await statusOf(without.deleteFile(trashed, { trash: true })),
        ).toBe(403);
        expect(await statusOf(without.emptyTrash())).toBe(403);
        // Refused deletions leave the files in place.
        expect(existsSync(permanent) && existsSync(trashed)).toBe(true);
        expect(await statusOf(withDelete.deleteFile(permanent))).toBe(200);
        expect(existsSync(permanent)).toBe(false);

        if (process.platform === "linux") {
            // Trash and emptying the trash use the same permission.
            expect(
                await statusOf(withDelete.deleteFile(trashed, { trash: true })),
            ).toBe(200);
            expect(await statusOf(withDelete.emptyTrash())).toBe(200);
        }
    });

    it("commands:execute gates exec", async () => {
        const json = JSON.stringify({ argv: ["true"], cwd: null, env: {} });
        const path = `/api/v1/agents/${agentA}/exec`;

        // Every file permission together still does not allow running commands.
        expect(
            await rawStatus({
                bearer: await token(["files:read", "files:write", "files:delete"]),
                method: "POST",
                path,
                json,
            }),
        ).toBe(403);
        expect(
            await rawStatus({
                bearer: await token(["commands:execute"]),
                method: "POST",
                path,
                json,
            }),
        ).toBe(200);
    });

    it("agents:manage gates starting and stopping managed agents", async () => {
        const without = agentAs(await token(ALL_PERMISSIONS.filter((permission) => permission !== "agents:manage")), managedAgent);
        const withManage = agentAs(await token(["agents:manage"]), managedAgent);

        // Lifecycle control needs agents:manage.
        expect(await statusOf(without.start())).toBe(403);
        expect(await statusOf(withManage.start())).toBe(200);
        // Stopping is the same permission.
        expect(await statusOf(without.shutdown())).toBe(403);
        expect(await statusOf(withManage.shutdown())).toBe(200);
    });
});

describe("Access token device scope", () => {
    it("limits inventory, details and files to the scoped devices", async () => {
        const bearer = await token(["files:read"], {
            kind: "only",
            device_ids: [agentA],
        });

        const client = clientAs(bearer);
        const listed = (await client.listAgents()).map((agent) => agent.id);

        // Inventory lists only in-scope devices.
        expect(listed).toEqual([agentA]);
        // Out-of-scope device details and files are refused.
        expect(
            await rawStatus({
                bearer,
                method: "GET",
                path: `/api/v1/agents/${agentB}`,
            }),
        ).toBe(403);
        expect(await statusOf(agentAs(bearer, agentB).ls(rootB))).toBe(403);
        // In-scope details work.
        expect(
            await rawStatus({
                bearer,
                method: "GET",
                path: `/api/v1/agents/${agentA}`,
            }),
        ).toBe(200);
    });

    it("needs read on the source and write on the destination for copies", async () => {
        const source = join(rootA, "copy-source.txt");
        writeFileSync(source, "copied");
        const readBoth = await token(["files:read"]);

        const readAWriteB = await token(["files:read", "files:write"], {
            kind: "only",
            device_ids: [agentA, agentB],
        });

        // Read alone cannot write the destination.
        expect(
            await statusOf(
                agentAs(readBoth, agentA).copyTo(
                    { agent: agentB, path: join(rootB, "denied-copy.txt") },
                    source,
                ),
            ),
        ).toBe(403);

        const started = await agentAs(readAWriteB, agentA).copyTo(
            { agent: agentB, path: join(rootB, "allowed-copy.txt") },
            source,
        );

        // Read on the source plus write on the destination admits the copy.
        expect(started.copy_request_id).toBeGreaterThan(0);
        await waitForValue({
            description: "scoped copy completion",
            predicate: async () =>
                existsSync(join(rootB, "allowed-copy.txt")) ? true : undefined,
        });
        // The copy transfer is visible to the token that can see both devices.
        expect(
            (await clientAs(readAWriteB).getTransferProgress()).transfers.some(
                (entry) => entry.request_id === started.copy_request_id,
            ),
        ).toBe(true);

        const onlyA = await token(["files:read"], {
            kind: "only",
            device_ids: [agentA],
        });

        // Transfer progress hides rows that touch a device outside the scope.
        expect(
            (await clientAs(onlyA).getTransferProgress()).transfers.some(
                (entry) => entry.request_id === started.copy_request_id,
            ),
        ).toBe(false);
        // Cancelling or pausing an invisible transfer reveals nothing about it.
        expect(
            await statusOf(
                clientAs(onlyA).cancelTransfer(started.copy_request_id),
            ),
        ).toBe(404);
        expect(
            await statusOf(
                clientAs(onlyA).pauseTransfer(started.copy_request_id),
            ),
        ).toBe(404);
    });

    it("needs delete on the source for moves", async () => {
        const source = join(rootA, "move-source.txt");
        writeFileSync(source, "moved");
        const readWrite = await token(["files:read", "files:write"]);
        const full = await token(["files:read", "files:write", "files:delete"]);
        const destination = { agent: agentB, path: join(rootB, "moved.txt") };

        // A move removes the source, so read and write are not enough.
        expect(
            await statusOf(agentAs(readWrite, agentA).moveTo(destination, source)),
        ).toBe(403);
        // With delete the move is admitted and completes.
        expect(
            await statusOf(agentAs(full, agentA).moveTo(destination, source)),
        ).toBe(200);
        await waitForValue({
            description: "scoped move completion",
            predicate: async () =>
                existsSync(destination.path) && !existsSync(source)
                    ? true
                    : undefined,
        });
    });

    it("needs read on both sides for diffs", async () => {
        const left = join(rootA, "diff-left.txt");
        const right = join(rootB, "diff-right.txt");
        writeFileSync(left, "left\n");
        writeFileSync(right, "right\n");

        const json = JSON.stringify({
            left: { agent: agentA, path: left },
            right: { agent: agentB, path: right },
        });

        const onlyA = await token(["files:read"], {
            kind: "only",
            device_ids: [agentA],
        });

        // Reading one side is not enough to diff against the other.
        expect(
            await rawStatus({
                bearer: onlyA,
                method: "POST",
                path: "/api/v1/diff",
                json,
            }),
        ).toBe(403);
        // With read on both devices the diff works.
        expect(
            await rawStatus({
                bearer: await token(["files:read"]),
                method: "POST",
                path: "/api/v1/diff",
                json,
            }),
        ).toBe(200);
    });
});

describe("Session-only routes and authentication", () => {
    it("refuses server, agent and account management to any token", async () => {
        const bearer = await token(ALL_PERMISSIONS);

        // Creating agents could add a local agent and widen access, so it is session-only.
        expect(
            await rawStatus({
                bearer,
                method: "POST",
                path: "/api/v1/agents",
                json: JSON.stringify({ name: "x", target: "x" }),
            }),
        ).toBe(403);
        // Restarting the server is session-only and must not happen.
        expect(
            await rawStatus({
                bearer,
                method: "POST",
                path: "/api/v1/server/restart",
            }),
        ).toBe(403);
        // Server logging and the account's UI state are session-only.
        expect(
            await rawStatus({
                bearer,
                method: "PUT",
                path: "/api/v1/server/logging-level",
                json: JSON.stringify({ level: "debug" }),
            }),
        ).toBe(403);
        expect(
            await rawStatus({ bearer, method: "GET", path: "/api/v1/user/state" }),
        ).toBe(403);
        // The server is still the same process: the session sees it running.
        expect(await statusOf(api.getServerInfo())).toBe(200);

        const statusCode = await new Promise<number>((resolve, reject) => {
            const socket = new WebSocket(
                `ws://127.0.0.1:${VITEST_SERVER_PORT}/api/v1/agents/${agentA}/terminal/ws`,
                { headers: { Authorization: `Bearer ${bearer}` } },
            );

            socket.on("unexpected-response", (_request, response) => {
                resolve(response.statusCode ?? 0);
                socket.terminate();
            });
            socket.on("open", () => {
                socket.close();
                reject(new Error("Terminal websocket accepted a token"));
            });
            socket.on("error", () => {});
        });

        // Terminals are session-only even for a token with commands:execute.
        expect(statusCode).toBe(403);
    });

    it("lets a bearer token decide over a valid cookie", async () => {
        const response = await fetch(`${baseUrl}/api/v1/agents`, {
            headers: {
                ...api.getAuthHeaders(),
                Authorization: "Bearer redoor_at_invalid",
            },
        });

        // An invalid token is rejected even when a valid session cookie is also sent.
        expect(response.status).toBe(401);
    });

    it("requires a credential and denies unclassified routes to tokens", async () => {
        // No credential at all is rejected.
        expect((await fetch(`${baseUrl}/api/v1/agents`)).status).toBe(401);
        // Protocol mismatches are not part of any token permission.
        expect(
            await rawStatus({
                bearer: await token(ALL_PERMISSIONS),
                method: "GET",
                path: "/api/v1/protocol-mismatches",
            }),
        ).toBe(403);
    });
});
