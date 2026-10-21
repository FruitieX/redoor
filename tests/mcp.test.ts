import {
    mkdirSync,
    mkdtempSync,
    readFileSync,
    rmSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it, onTestFinished } from "vitest";

import { ApiClient, type Permission } from "#ui/api-client";
import {
    ALL_PERMISSIONS,
    ProcessManager,
    TEST_AGENT_TOKEN,
    createAccessToken,
    TEST_APP_NAME,
    TEST_PASSWORD,
    TEST_USERNAME,
    VITEST_SERVER_PORT,
    waitForAgentReady,
    waitForLogMessage,
    waitForPort,
    waitForValue,
} from "./test-utils";
import {
    type JsonObject,
    MCP_PROTOCOL_VERSION,
    createMcpTestClient,
} from "./mcp-client";

import { registerMcpTransferRegressionTests } from "./mcp-transfer-regressions";

const processManager = new ProcessManager();

const baseUrl = `http://127.0.0.1:${VITEST_SERVER_PORT}`;

const endpoint = `${baseUrl}/mcp`;

const allowedOrigin = "https://allowed-mcp-client.example";

const temporaryDirectory = mkdtempSync(join(tmpdir(), "redoor-mcp-test-"));

// Operation quotas include persisted history, so each suite needs fresh state across runs.
const serverHome = join(temporaryDirectory, "server-home");

const configPath = join(temporaryDirectory, "config.toml");

const api = new ApiClient(baseUrl);

let activeToken: string;

let deviceToken: string;

let secondDeviceToken: string;

let serverPid: number;

const testDeviceId = "mcp-test-agent";

const secondDeviceId = "mcp-test-agent-two";

const managedDeviceId = "mcp-managed-agent";

const agentRoot = join(temporaryDirectory, "agent-root");

const secondAgentRoot = join(temporaryDirectory, "agent-root-two");

type RawMcpRequest = {
    jsonrpc: string;
    id: string;
    method: string;
    params: JsonObject;
};

/** Creates a token through the REST API and returns its one-time secret and id. */
async function createToken(options: {
    name: string;
    expiresAt?: string;
    permissions?: Permission[];
    devices?: string[];
}): Promise<{ token: string; tokenId: string }> {
    const created = await createAccessToken(api, {
        name: options.name,
        expiresAt: options.expiresAt,
        permissions: options.permissions ?? ["files:read"],
        devices:
            options.devices === undefined
                ? { kind: "all" }
                : { kind: "only", device_ids: options.devices },
    });

    return { token: created.token, tokenId: created.metadata.id };
}

/** Creates a token with every permission on every test device and returns its secret. */
async function createOperatorToken(name: string): Promise<string> {
    const devices = [testDeviceId, secondDeviceId, managedDeviceId];

    return (
        await createToken({ name, permissions: ALL_PERMISSIONS, devices })
    ).token;
}

/** Sends deliberately invalid protocol requests that the normal helper refuses to construct. */
async function rawMcp(options: {
    token?: string;
    body?: unknown;
    headers?: Record<string, string>;
    method?: string;
}): Promise<Response> {
    const headers = new Headers(options.headers);

    if (options.token !== undefined) {
        headers.set("Authorization", `Bearer ${options.token}`);
    }

    if (options.body !== undefined) {
        headers.set("Content-Type", "application/json");
    }

    return fetch(endpoint, {
        method: options.method ?? "POST",
        headers,
        body:
            options.body === undefined
                ? undefined
                : JSON.stringify(options.body),
    });
}

/** Constructs one valid request body so individual rejection tests alter only one condition. */
function requestBody(
    method: string,
    params: JsonObject = {},
    protocolVersion: string = MCP_PROTOCOL_VERSION,
): RawMcpRequest {
    return {
        jsonrpc: "2.0",
        id: "raw-1",
        method,
        params: {
            ...params,
            _meta: {
                "io.modelcontextprotocol/protocolVersion": protocolVersion,
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    };
}

beforeAll(async () => {
    // Keep normal operation retention: a short TTL can expire completed transfers before
    // their first poll under load. Deterministic registry tests cover expiry separately.
    process.env.REDOOR_MCP_OPERATION_JOB_QUOTA = "512";
    process.env.REDOOR_MCP_OPERATION_GLOBAL_QUOTA = "4096";
    mkdirSync(agentRoot);
    mkdirSync(secondAgentRoot);
    writeFileSync(
        configPath,
        `agent_token = "${TEST_AGENT_TOKEN}"

[server]
username = "${TEST_USERNAME}"
password = "${TEST_PASSWORD}"
mcp_allowed_origins = ["${allowedOrigin}"]

[[agents]]
local = true
name = "${managedDeviceId}"
home = "${agentRoot}"
log = "${join(temporaryDirectory, "managed-agent.log")}"
`,
    );
    process.env.REDOOR_PORT = VITEST_SERVER_PORT.toString();
    serverPid = processManager.spawnServer({
        config: configPath,
        home: serverHome,
    });
    await waitForPort(VITEST_SERVER_PORT);
    await api.login(TEST_USERNAME, TEST_PASSWORD);
    // A read-only token limited to the dormant managed device.
    activeToken = (
        await createToken({
            name: "primary test client",
            devices: [managedDeviceId],
        })
    ).token;
    const server = processManager.getProcess(serverPid);

    if (server === undefined) throw new Error("MCP test server missing");

    const registered = waitForLogMessage(
        server,
        new RegExp(`Agent registered:.*agent_name=${testDeviceId}`),
        10_000,
    );

    const transferred = waitForAgentReady(server, testDeviceId);
    processManager.spawnAgent({
        wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}`,
        name: testDeviceId,
        cwd: agentRoot,
        home: agentRoot,
    });
    await Promise.all([registered, transferred]);

    const secondRegistered = waitForLogMessage(
        server,
        new RegExp(`Agent registered:.*agent_name=${secondDeviceId}`),
        10_000,
    );

    const secondTransferred = waitForAgentReady(server, secondDeviceId);
    processManager.spawnAgent({
        wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}`,
        name: secondDeviceId,
        cwd: secondAgentRoot,
        home: secondAgentRoot,
    });
    await Promise.all([secondRegistered, secondTransferred]);

    deviceToken = await createOperatorToken("device operator");
    secondDeviceToken = await createOperatorToken("other device operator");
}, 30_000);

afterAll(async () => {
    await processManager.killAll();
    delete process.env.REDOOR_MCP_OPERATION_JOB_QUOTA;
    delete process.env.REDOOR_MCP_OPERATION_GLOBAL_QUOTA;
    rmSync(temporaryDirectory, { recursive: true, force: true });
});

describe("stateless authenticated MCP", () => {
    it("discovers and lists only implemented capabilities", async () => {
        const client = createMcpTestClient({ endpoint, token: activeToken });
        const discovery = await client.discover();
        // Discovery must identify this exact stateless revision on every independent request.
        expect(discovery.result).toMatchObject({
            resultType: "complete",
            supportedVersions: [MCP_PROTOCOL_VERSION],
            capabilities: { tools: {} },
        });
        // Unimplemented resources, prompts, subscriptions, and client interactions must stay absent.
        expect(discovery.result.capabilities).toEqual({ tools: {} });

        const tools = await client.listTools();
        const names = tools.result.tools.map((tool) => tool.name);
        // The harmless smoke test remains available beside the device-operation catalog.
        expect(
            tools.result.tools.find((tool) => tool.name === "hello"),
        ).toMatchObject({
            name: "hello",
            annotations: { readOnlyHint: true, destructiveHint: false },
        });
        // A read-only token sees its read tools but nothing it could never use.
        expect(names).toEqual(
            expect.arrayContaining(["list_devices", "read_file", "search_contents"]),
        );
        expect(names).not.toContain("run_command");
        expect(names).not.toContain("write_file");
        expect(names).not.toContain("delete_path");

        const operator = createMcpTestClient({ endpoint, token: deviceToken });

        const operatorNames = (await operator.listTools()).result.tools.map(
            (tool) => tool.name,
        );

        // A full-permission token sees the whole catalog.
        expect(operatorNames).toHaveLength(19);
        expect(operatorNames).toContain("run_command");
        // Big local file transfers belong to the remote CLI, so MCP no longer offers them.
        expect(operatorNames).not.toContain("prepare_file_download");
        expect(operatorNames).not.toContain("prepare_file_upload");
    });

    it("enforces device grants and performs file, search, command, and transfer workflows", async () => {
        const client = createMcpTestClient({ endpoint, token: deviceToken });
        const limited = createMcpTestClient({ endpoint, token: activeToken });
        // Inventory needs no permission but only lists devices in the token's scope.
        expect(
            (await limited.callTool("list_devices")).result.structuredContent
                ?.devices,
        ).toEqual([expect.objectContaining({ device_id: managedDeviceId })]);
        // Devices outside the scope are refused, not just hidden.
        await expect(
            limited.callTool("get_device", { device_id: testDeviceId }),
        ).rejects.toThrow("(403)");
        const inventory = await client.callTool("list_devices");
        expect(inventory.result.structuredContent?.devices).toEqual(
            expect.arrayContaining([
                expect.objectContaining({
                    device_id: testDeviceId,
                    supported_features: {
                        trash: expect.any(Boolean),
                        trash_restore: expect.any(Boolean),
                    },
                }),
                expect.objectContaining({ device_id: secondDeviceId }),
                expect.objectContaining({
                    device_id: managedDeviceId,
                    managed: true,
                }),
            ]),
        );

        const firstPath = join(agentRoot, "mcp-source.txt");
        const copyPath = join(agentRoot, "mcp-copy.txt");
        const movedPath = join(secondAgentRoot, "mcp-moved.txt");
        await client.callTool("write_file", {
            device_id: testDeviceId,
            path: firstPath,
            content: "alpha MCP marker",
            on_existing: "error",
        });
        // Existing destinations fail closed unless the caller explicitly chooses replacement.
        await expect(
            client.callTool("write_file", {
                device_id: testDeviceId,
                path: firstPath,
                content: "must not replace",
                on_existing: "error",
            }),
        ).rejects.toThrow("(409)");
        const directoryPath = join(agentRoot, "created-by-mcp");
        await client.callTool("create_directory", {
            device_id: testDeviceId,
            path: directoryPath,
        });
        expect(
            (
                await client.callTool("list_directory", {
                    device_id: testDeviceId,
                    path: agentRoot,
                })
            ).result.structuredContent?.entries,
        ).toEqual(
            expect.arrayContaining([
                expect.objectContaining({ name: "created-by-mcp" }),
                expect.objectContaining({ name: "mcp-source.txt" }),
            ]),
        );
        expect(
            (
                await client.callTool("stat_path", {
                    device_id: testDeviceId,
                    path: firstPath,
                })
            ).result.structuredContent,
        ).toMatchObject({ is_file: true, size: 16 });
        expect(
            (
                await client.callTool("search_paths", {
                    device_id: testDeviceId,
                    root: agentRoot,
                    query: "mcp-source",
                })
            ).result.structuredContent?.matches,
        ).toEqual([expect.objectContaining({ path: firstPath })]);

        const read = await client.callTool("read_file", {
            device_id: testDeviceId,
            path: firstPath,
            encoding: "text",
        });

        // Inline reads report exact content and an offset suitable for subsequent chunks.
        expect(read.result.structuredContent).toMatchObject({
            content: "alpha MCP marker",
            more: false,
        });
        expect(
            (
                await client.callTool("search_contents", {
                    device_id: testDeviceId,
                    root: agentRoot,
                    query: "MCP marker",
                    fixed_string: true,
                })
            ).result.structuredContent?.results,
        ).toEqual([expect.objectContaining({ path: firstPath })]);

        const copy = await client.callTool<
            JsonObject & { operation_id: string }
        >("copy_path", {
            source: { device_id: testDeviceId, path: firstPath },
            destination: { device_id: testDeviceId, path: copyPath },
            on_existing: "error",
            idempotency_key: "copy-once",
        });

        const copyRetry = await client.callTool("copy_path", {
            source: { device_id: testDeviceId, path: firstPath },
            destination: { device_id: testDeviceId, path: copyPath },
            on_existing: "error",
            idempotency_key: "copy-once",
        });

        // Retrying the normalized mutation returns the original admission instead of copying twice.
        expect(copyRetry.result.structuredContent?.operation_id).toBe(
            copy.result.structuredContent?.operation_id,
        );
        const copyOperationId = copy.result.structuredContent?.operation_id;

        if (copyOperationId === undefined)
            throw new Error("Copy operation missing");
        await client.waitForOperation(copyOperationId);

        const command = await client.callTool<
            JsonObject & { operation_id: string }
        >("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", "printf command-output; exit 7"],
            cwd: agentRoot,
            mode: "foreground",
            output_limit: 4096,
        });

        const operationId = command.result.structuredContent?.operation_id;

        if (operationId === undefined)
            throw new Error("Command operation missing");

        const retainedOutput = await client.callTool<
            JsonObject & { content: string }
        >("read_command_output", { operation_id: operationId });

        // Only the agent's terminal acknowledgement supplies the real exit status.
        expect(command.result.structuredContent).toMatchObject({
            state: "completed",
            termination: { type: "exit", code: 7 },
        });

        // Output pages retain binary-safe event records independently of terminal status.
        const retainedEvents = Buffer.from(
            retainedOutput.result.structuredContent?.content ?? "",
            "base64",
        ).toString();

        expect(retainedEvents).toContain('"type":"stdout"');
        expect(retainedEvents).toContain(
            Buffer.from("command-output").toJSON().data.join(","),
        );

        const other = createMcpTestClient({
            endpoint,
            token: secondDeviceToken,
        });

        // Device permission alone cannot disclose another token's retained output.
        await expect(
            other.callTool("read_command_output", {
                operation_id: operationId,
            }),
        ).rejects.toThrow("(403)");

        const uploadedPath = join(agentRoot, "inline-upload.txt");
        await client.callTool("write_file", {
            device_id: testDeviceId,
            path: uploadedPath,
            content: "inline upload",
            on_existing: "error",
        });
        const sameDeviceMoved = join(agentRoot, "inline-upload-moved.txt");

        const sameMove = await client.callTool<
            JsonObject & { operation_id: string }
        >("move_path", {
            source: { device_id: testDeviceId, path: uploadedPath },
            destination: { device_id: testDeviceId, path: sameDeviceMoved },
            on_existing: "error",
        });

        const sameMoveId = sameMove.result.structuredContent?.operation_id;

        if (sameMoveId === undefined)
            throw new Error("Same-device move operation missing");
        expect(
            (await client.waitForOperation(sameMoveId)).result
                .structuredContent,
        ).toMatchObject({ state: "completed", termination: { atomic: true } });

        const move = await client.callTool<
            JsonObject & { operation_id: string }
        >("move_path", {
            source: { device_id: testDeviceId, path: copyPath },
            destination: { device_id: secondDeviceId, path: movedPath },
            on_existing: "error",
        });

        const moveOperationId = move.result.structuredContent?.operation_id;

        if (moveOperationId === undefined)
            throw new Error("Move operation missing");
        const completedMove = await client.waitForOperation(moveOperationId);
        // Cross-device move completion means copy publication and guarded source cleanup finished.
        expect(completedMove.result.structuredContent?.state).toBe("completed");
        expect(
            (
                await client.callTool("read_file", {
                    device_id: secondDeviceId,
                    path: movedPath,
                    encoding: "text",
                })
            ).result.structuredContent?.content,
        ).toBe("alpha MCP marker");

        await client.callTool("start_managed_agent", {
            device_id: managedDeviceId,
        });
        let managedConnected = false;

        for (
            let attempt = 0;
            attempt < 100 && !managedConnected;
            attempt += 1
        ) {
            const device = await client.callTool<
                JsonObject & { device: JsonObject & { status: string } }
            >("get_device", { device_id: managedDeviceId });

            managedConnected =
                device.result.structuredContent?.device.status === "connected";

            if (!managedConnected)
                await new Promise<void>((resolve) => setTimeout(resolve, 20));
        }

        expect(managedConnected).toBe(true);
        expect(
            (
                await client.callTool("stop_managed_agent", {
                    device_id: managedDeviceId,
                })
            ).result.structuredContent,
        ).toMatchObject({ cleanup_complete: true });
        await client.callTool("delete_path", {
            device_id: testDeviceId,
            path: firstPath,
            mode: "permanent",
            recursive: true,
        });
    });

    registerMcpTransferRegressionTests({
        endpoint,
        baseUrl,
        token: () => deviceToken,
        sourceDevice: testDeviceId,
        destinationDevice: secondDeviceId,
        sourceRoot: agentRoot,
        destinationRoot: secondAgentRoot,
        metadataPath: join(
            serverHome,
            ".local/share",
            TEST_APP_NAME,
            "mcp-operations/operations.json",
        ),
    });

    it("bounds retained output, cancels background execution, and keeps searches independent", async () => {
        // The per-credential rolling request budget is shared by every test using the same
        // token, so this request-heavy test owns credentials instead of inheriting the budget
        // the preceding workflow test already spent.
        const [token, peerToken] = await Promise.all([
            createOperatorToken("retention operator"),
            createOperatorToken("retention peer"),
        ]);

        const client = createMcpTestClient({ endpoint, token });
        const peerClient = createMcpTestClient({ endpoint, token: peerToken });

        const large = await client.callTool("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", "head -c 4096 /dev/zero"],
            cwd: agentRoot,
            mode: "foreground",
            output_limit: 4096,
        });

        // The configured per-job quota truncates retention while the process still reaches exit.
        expect(large.result.structuredContent).toMatchObject({
            state: "completed",
            output_size: 512,
            output_truncated: true,
        });

        // Ten parallel admissions exceed one credential's concurrency cap of eight, so they are
        // split across two credentials; the retention ceiling under test is global anyway.
        const quotaOperations = await Promise.all(
            Array.from({ length: 10 }, (_, index) =>
                (index % 2 === 0 ? client : peerClient).callTool<
                    JsonObject & { operation_id: string }
                >("run_command", {
                    device_id: testDeviceId,
                    argv: ["sh", "-lc", "head -c 4096 /dev/zero"],
                    cwd: agentRoot,
                    mode: "foreground",
                    output_limit: 4096,
                }),
            ),
        );

        // Parallel admissions cannot collectively retain more than the configured global ceiling.
        expect(
            quotaOperations.some(
                (operation) =>
                    operation.result.structuredContent?.output_size === 0 &&
                    operation.result.structuredContent?.output_truncated ===
                        true,
            ),
        ).toBe(true);

        const largeDirectory = join(agentRoot, "large-directory");
        await client.callTool("run_command", {
            device_id: testDeviceId,
            argv: [
                "sh",
                "-lc",
                `mkdir -p '${largeDirectory}'; i=0; while [ "$i" -lt 2050 ]; do : > '${largeDirectory}/entry-'"$i"; i=$((i+1)); done`,
            ],
            cwd: agentRoot,
            mode: "foreground",
            output_limit: 128,
        });

        const listing = await client.callTool("list_directory", {
            device_id: testDeviceId,
            path: largeDirectory,
        });

        expect(listing.result.structuredContent).toMatchObject({
            truncated: true,
        });
        expect(listing.result.structuredContent?.entries).toHaveLength(2000);

        const background = await client.callTool<
            JsonObject & { operation_id: string }
        >("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", "exec tail -f /dev/null"],
            cwd: agentRoot,
            mode: "background",
            output_limit: 128,
        });

        const operationId = background.result.structuredContent?.operation_id;

        if (operationId === undefined)
            throw new Error("Background operation missing");
        expect(
            (
                await client.callTool("cancel_operation", {
                    operation_id: operationId,
                })
            ).result.structuredContent,
        ).toMatchObject({ cancellation_accepted: true });
        expect(
            (await client.waitForOperation(operationId)).result
                .structuredContent?.state,
        ).toBe("canceled");

        const searches = await Promise.all(
            Array.from({ length: 8 }, (_, index) =>
                client.callTool("search_contents", {
                    device_id: testDeviceId,
                    root: agentRoot,
                    query: index % 2 === 0 ? "alpha" : "streamed",
                    fixed_string: true,
                }),
            ),
        );

        // Each request owns its cancellation token, so concurrent UI/MCP-style scans survive.
        expect(
            searches.every(
                (search) =>
                    search.result.structuredContent?.cancelled === false,
            ),
        ).toBe(true);
    }, 30_000);

    it("enforces both transfer endpoints and serves no big-file endpoints", async () => {
        const sourceOnly = await createToken({
            name: "source-only transfer",
            permissions: ["files:read", "files:write", "files:delete"],
            devices: [testDeviceId],
        });

        const destinationOnly = await createToken({
            name: "destination-only transfer",
            permissions: ["files:read", "files:write", "files:delete"],
            devices: [secondDeviceId],
        });

        const sourceClient = createMcpTestClient({
            endpoint,
            token: sourceOnly.token,
        });

        const destinationClient = createMcpTestClient({
            endpoint,
            token: destinationOnly.token,
        });

        const sourcePath = join(agentRoot, "inline-upload-moved.txt");
        const destinationPath = join(secondAgentRoot, "unauthorized-copy.txt");
        // A source scope cannot infer or write a destination outside its device scope.
        await expect(
            sourceClient.callTool("copy_path", {
                source: { device_id: testDeviceId, path: sourcePath },
                destination: {
                    device_id: secondDeviceId,
                    path: destinationPath,
                },
                on_existing: "error",
            }),
        ).rejects.toThrow("(403)");
        // Destination authority alone cannot read the source side of a transfer.
        await expect(
            destinationClient.callTool("copy_path", {
                source: { device_id: testDeviceId, path: sourcePath },
                destination: {
                    device_id: secondDeviceId,
                    path: destinationPath,
                },
                on_existing: "error",
            }),
        ).rejects.toThrow("(403)");

        const removedEndpoint = `${baseUrl}/mcp/files/${testDeviceId}${sourcePath}`;
        const authorization = { Authorization: `Bearer ${sourceOnly.token}` };

        const download = await fetch(removedEndpoint, {
            headers: authorization,
        });

        // The old streaming side endpoint is gone; only the UI fallback answers, never with file bytes.
        expect(await download.text()).not.toContain("inline upload");

        const upload = await fetch(removedEndpoint, {
            method: "PUT",
            headers: authorization,
            body: "replaced through a removed endpoint",
        });

        // Uploads are refused, and the file keeps its content.
        expect(upload.ok).toBe(false);
        expect(readFileSync(sourcePath, "utf8")).toBe("inline upload");
    });

    it("shows devices added later to an all-devices token", async () => {
        const created = await createToken({ name: "all devices" });
        const client = createMcpTestClient({ endpoint, token: created.token });
        const lateDeviceId = "mcp-late-agent";
        const lateRoot = join(temporaryDirectory, "late-agent-root");
        mkdirSync(lateRoot, { recursive: true });

        const before = (await client.callTool("list_devices")).result
            .structuredContent?.devices;

        // The device does not exist yet, so it cannot be listed.
        expect(before).not.toEqual(
            expect.arrayContaining([
                expect.objectContaining({ device_id: lateDeviceId }),
            ]),
        );
        const server = processManager.getProcess(serverPid);

        if (server === undefined) throw new Error("MCP test server missing");
        const ready = waitForAgentReady(server, lateDeviceId);

        const agentPid = processManager.spawnAgent({
            wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}`,
            name: lateDeviceId,
            cwd: lateRoot,
            home: lateRoot,
        });

        onTestFinished(async () => {
            await processManager.killAndWait(agentPid);
        });
        await ready;

        // An all-devices scope covers devices that connect after the token was created.
        await waitForValue({
            description: "late agent in the all-devices inventory",
            predicate: async () => {
                const listed = await client.callTool<
                    JsonObject & { devices: Array<{ device_id: string }> }
                >("list_devices");

                return listed.result.structuredContent?.devices.some(
                    (device) => device.device_id === lateDeviceId,
                )
                    ? true
                    : undefined;
            },
        });
    });

    it("recovers admitted background work as interrupted after server restart", async () => {
        const client = createMcpTestClient({ endpoint, token: deviceToken });

        const started = await client.callTool<
            JsonObject & { operation_id: string }
        >("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", "exec tail -f /dev/null"],
            cwd: agentRoot,
            mode: "background",
            output_limit: 128,
        });

        const operationId = started.result.structuredContent?.operation_id;

        if (operationId === undefined)
            throw new Error("Crash-ambiguity operation missing");
        await processManager.killAndWait(serverPid);
        serverPid = processManager.spawnServer({
            config: configPath,
            home: serverHome,
        });
        await waitForPort(VITEST_SERVER_PORT);

        const recovered = await createMcpTestClient({
            endpoint,
            token: deviceToken,
        }).getOperation(operationId);

        // Restart never claims the unacknowledged remote side effect succeeded or repeats it.
        expect(recovered.result.structuredContent).toMatchObject({
            state: "interrupted",
            termination: {
                reason: "server_restarted_before_terminal_acknowledgement",
            },
        });

        const cancellation = await client.callTool("cancel_operation", {
            operation_id: operationId,
        });

        // Router counters reset on restart; stale work must never send a new cancellation.
        expect(cancellation.result.structuredContent).toMatchObject({
            cancellation_accepted: false,
            termination_acknowledged: true,
        });
    });

    it("allows an authenticated hello call as the first request", async () => {
        const client = createMcpTestClient({ endpoint, token: activeToken });
        const response = await client.callTool("hello", { name: "Ada" });
        // Direct calls prove neither discovery nor initialization creates hidden session state.
        expect(response.result).toMatchObject({
            resultType: "complete",
            isError: false,
            structuredContent: { greeting: "Hello, Ada, from Redoor!" },
            content: [{ type: "text", text: "Hello, Ada, from Redoor!" }],
        });
    });

    it("requires access tokens despite the non-API public-path exception", async () => {
        const body = requestBody("server/discover");

        const headers = {
            "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
            "Mcp-Method": "server/discover",
        };

        const missing = await rawMcp({ body, headers });
        // `/mcp` must not inherit the existing middleware's public non-API exception.
        expect(missing.status).toBe(401);
        expect(missing.headers.get("www-authenticate")).toContain("Bearer");

        const invalid = await rawMcp({ token: "not-a-token", body, headers });
        // Invalid opaque material is indistinguishable from an unknown credential.
        expect(invalid.status).toBe(401);

        const cookieOnly = await rawMcp({
            body,
            headers: { ...headers, Cookie: api.getAuthHeaders().Cookie ?? "" },
        });

        // Even a valid browser session cookie cannot authenticate MCP; only access tokens do.
        expect(cookieOnly.status).toBe(401);

        const publicPage = await fetch(`${baseUrl}/not-an-api-route`);
        // This confirms the neighboring legacy non-API behavior still exists and MCP overrides it.
        expect(publicPage.status).toBe(200);
    });

    it("observes REST deletion on the next request without a restart", async () => {
        const created = await createToken({ name: "revocation test" });
        const client = createMcpTestClient({ endpoint, token: created.token });
        // A newly created token must be immediately usable.
        expect((await client.callTool("hello")).result.resultType).toBe(
            "complete",
        );
        await api.deleteAccessToken(created.tokenId);
        await expect(client.callTool("hello")).rejects.toThrow("(401)");
    });

    it("rejects expired credentials", async () => {
        const expiresAt = new Date(Date.now() + 1_000).toISOString();
        const created = await createToken({ name: "expiry test", expiresAt });
        const client = createMcpTestClient({ endpoint, token: created.token });
        // The credential begins valid so the test distinguishes expiry from creation failure.
        expect((await client.callTool("hello")).result.resultType).toBe(
            "complete",
        );
        let expired = false;

        for (let attempt = 0; attempt < 100 && !expired; attempt += 1) {
            try {
                await client.callTool("hello");
            } catch (error) {
                expired = String(error).includes("(401)");
            }

            if (!expired) {
                await new Promise<void>((resolve) => setTimeout(resolve, 20));
            }
        }

        // Authentication checks expiry on every request instead of caching an accepted token.
        expect(expired).toBe(true);
    });

    it("validates configured browser origins while permitting native clients", async () => {
        const body = requestBody("server/discover");

        const common = {
            "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
            "Mcp-Method": "server/discover",
        };

        const rejected = await rawMcp({
            token: activeToken,
            body,
            headers: { ...common, Origin: "https://attacker.example" },
        });

        // A present unconfigured Origin is rejected before protocol dispatch.
        expect(rejected.status).toBe(403);

        const allowed = await rawMcp({
            token: activeToken,
            body,
            headers: { ...common, Origin: allowedOrigin },
        });

        // Explicitly configured browser clients are allowed alongside origin-less native clients.
        expect(allowed.status).toBe(200);
        expect(allowed.headers.get("access-control-allow-origin")).toBe(
            allowedOrigin,
        );
    });

    it("enforces request metadata and mirrored headers", async () => {
        const missingMetadata = requestBody("tools/list");
        delete missingMetadata.params._meta;

        const metadataResponse = await rawMcp({
            token: activeToken,
            body: missingMetadata,
            headers: {
                "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
                "Mcp-Method": "tools/list",
            },
        });

        // Stateless processing requires capabilities/version on this exact request.
        expect(metadataResponse.status).toBe(400);
        expect((await metadataResponse.json()).error.code).toBe(-32602);

        const missingHeader = await rawMcp({
            token: activeToken,
            body: requestBody("tools/list"),
            headers: { "Mcp-Method": "tools/list" },
        });

        // Missing mirrored metadata uses the reserved HeaderMismatch error.
        expect(missingHeader.status).toBe(400);
        expect((await missingHeader.json()).error.code).toBe(-32020);

        const mismatchedName = await rawMcp({
            token: activeToken,
            body: requestBody("tools/call", {
                name: "hello",
                arguments: {},
            }),
            headers: {
                "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
                "Mcp-Method": "tools/call",
                "Mcp-Name": "another-tool",
            },
        });

        // The proxy-visible tool name cannot disagree with the operation the server executes.
        expect(mismatchedName.status).toBe(400);
        expect((await mismatchedName.json()).error.code).toBe(-32020);

        const unsupportedBody = requestBody("tools/list", {}, "1900-01-01");

        const unsupported = await rawMcp({
            token: activeToken,
            body: unsupportedBody,
            headers: {
                "MCP-Protocol-Version": "1900-01-01",
                "Mcp-Method": "tools/list",
            },
        });

        // Modern clients receive supported revisions rather than falling back to initialization.
        expect(unsupported.status).toBe(400);
        expect(await unsupported.json()).toMatchObject({
            error: {
                code: -32022,
                data: { supported: [MCP_PROTOCOL_VERSION] },
            },
        });
    });

    it("maps unknown methods, tools, and invalid arguments distinctly", async () => {
        const unknownMethod = await rawMcp({
            token: activeToken,
            body: requestBody("resources/list"),
            headers: {
                "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
                "Mcp-Method": "resources/list",
            },
        });

        // A JSON-RPC 404 identifies a modern endpoint with no resources method.
        expect(unknownMethod.status).toBe(404);
        expect((await unknownMethod.json()).error.code).toBe(-32601);

        const client = createMcpTestClient({ endpoint, token: activeToken });
        await expect(client.callTool("missing")).rejects.toThrow("-32602");
        await expect(
            client.callTool("hello", { unexpected: true }),
        ).rejects.toThrow("-32602");
    });

    it("keeps independent credentials and requests stateless", async () => {
        const second = await createToken({ name: "second independent client" });

        const firstClient = createMcpTestClient({
            endpoint,
            token: activeToken,
            metadata: {
                "io.modelcontextprotocol/clientInfo": {
                    name: "first-client",
                    version: "1",
                },
            },
        });

        const secondClient = createMcpTestClient({
            endpoint,
            token: second.token,
            metadata: {
                "io.modelcontextprotocol/clientInfo": {
                    name: "second-client",
                    version: "1",
                },
            },
        });

        const [first, other] = await Promise.all([
            firstClient.callTool("hello", { name: "One" }),
            secondClient.callTool("hello", { name: "Two" }),
        ]);

        // Self-reported client identity and connection timing do not cross-contaminate results.
        expect(first.result.structuredContent).toEqual({
            greeting: "Hello, One, from Redoor!",
        });
        expect(other.result.structuredContent).toEqual({
            greeting: "Hello, Two, from Redoor!",
        });
    });

    it("does not expose secrets in listings or server logs", async () => {
        const listed = JSON.stringify(await api.listAccessTokens());
        // Token metadata intentionally contains neither the one-time secret nor verifier fields.
        expect(listed).not.toContain(activeToken);
        expect(listed).not.toContain("verifier");

        const stored = readFileSync(
            join(
                serverHome,
                ".local/share",
                TEST_APP_NAME,
                "tokens/access-tokens.json",
            ),
            "utf8",
        );

        // Durable state contains only a one-way verifier, not the presented bearer value.
        expect(stored).not.toContain(activeToken);
        expect(processManager.getStdout(serverPid)).not.toContain(activeToken);
    });

    it("rejects legacy stream/session methods without minting session state", async () => {
        for (const method of ["GET", "DELETE"]) {
            const response = await rawMcp({
                token: activeToken,
                method,
                headers: { "Mcp-Session-Id": "legacy-session" },
            });

            // This revision has neither a standalone event stream nor DELETE session endpoint.
            expect(response.status).toBe(405);
            expect(response.headers.get("mcp-session-id")).toBeNull();
        }
    });
});
