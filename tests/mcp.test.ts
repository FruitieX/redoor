import {
    mkdirSync,
    mkdtempSync,
    readFileSync,
    renameSync,
    rmSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { $ } from "zx";
import {
    afterAll,
    beforeAll,
    describe,
    expect,
    it,
    onTestFinished,
} from "vitest";

import {
    ProcessManager,
    SERVER_PATH,
    TEST_AGENT_TOKEN,
    TEST_APP_NAME,
    TEST_PASSWORD,
    TEST_SERVER_HOME,
    TEST_USERNAME,
    VITEST_SERVER_PORT,
    waitForAgentTransfer,
    waitForLogMessage,
    waitForPort,
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

const configPath = join(temporaryDirectory, "config.toml");

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

/** Invokes the administrative CLI against the same application data as the test server. */
async function tokenCli(arguments_: string[]): Promise<string> {
    const result = await $({
        quiet: true,
        env: {
            ...process.env,
            HOME: TEST_SERVER_HOME,
            REDOOR_APP_NAME: TEST_APP_NAME,
        },
    })`${SERVER_PATH} server mcp-token ${arguments_}`;

    return result.stdout.trim();
}

/** Creates a token through the public bootstrap CLI and extracts its one-time secret. */
async function createToken(options: {
    name: string;
    expiresAt?: string;
    scopes?: string[];
    devices?: string[];
}): Promise<{ token: string; credentialId: string }> {
    const arguments_ = ["create", "--name", options.name];

    if (options.expiresAt !== undefined) {
        arguments_.push("--expires-at", options.expiresAt);
    }

    if (options.scopes?.length) {
        arguments_.push("--scopes", options.scopes.join(","));
    }

    if (options.devices?.length) {
        arguments_.push("--devices", options.devices.join(","));
    }

    const output: {
        token: string;
        credential: { credential_id: string };
    } = JSON.parse(await tokenCli(arguments_));

    return {
        token: output.token,
        credentialId: output.credential.credential_id,
    };
}

/** Creates a token with every operator scope on every test device and returns its secret. */
async function createOperatorToken(name: string): Promise<string> {
    const scopes = [
        "devices:read",
        "files:read",
        "files:write",
        "files:delete",
        "commands:execute",
        "agents:manage",
    ];

    const devices = [testDeviceId, secondDeviceId, managedDeviceId];

    return (await createToken({ name, scopes, devices })).token;
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
    process.env.REDOOR_MCP_OPERATION_JOB_QUOTA = "512";
    process.env.REDOOR_MCP_OPERATION_GLOBAL_QUOTA = "4096";
    process.env.REDOOR_MCP_OPERATION_EXPIRY_SECONDS = "2";
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
    serverPid = processManager.spawnServer({ config: configPath });
    await waitForPort(VITEST_SERVER_PORT);
    activeToken = (await createToken({ name: "primary test client" })).token;
    const server = processManager.getProcess(serverPid);

    if (server === undefined) throw new Error("MCP test server missing");

    const registered = waitForLogMessage(
        server,
        new RegExp(`Agent registered:.*agent_name=${testDeviceId}`),
        10_000,
    );

    const transferred = waitForAgentTransfer(server, testDeviceId);
    processManager.spawnAgent({
        wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}/ws`,
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

    const secondTransferred = waitForAgentTransfer(server, secondDeviceId);
    processManager.spawnAgent({
        wsAddress: `ws://127.0.0.1:${VITEST_SERVER_PORT}/ws`,
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
    delete process.env.REDOOR_MCP_OPERATION_EXPIRY_SECONDS;
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
        // The harmless smoke test remains available beside the device-operation catalog.
        expect(tools.result.tools).toHaveLength(21);
        expect(
            tools.result.tools.find((tool) => tool.name === "hello"),
        ).toMatchObject({
            name: "hello",
            annotations: { readOnlyHint: true, destructiveHint: false },
        });
    });

    it("enforces device grants and performs file, search, command, and transfer workflows", async () => {
        const client = createMcpTestClient({ endpoint, token: deviceToken });
        const denied = createMcpTestClient({ endpoint, token: activeToken });
        // Inventory requires its dedicated scope even though hello is available to every token.
        await expect(denied.callTool("list_devices")).rejects.toThrow("(403)");
        const inventory = await client.callTool("list_devices");
        expect(inventory.result.structuredContent?.devices).toEqual(
            expect.arrayContaining([
                expect.objectContaining({
                    device_id: testDeviceId,
                    supported_features: {
                        trash: expect.any(Boolean),
                        trash_restore: expect.any(Boolean),
                        self_exec: expect.any(Boolean),
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

        // Device permission alone cannot disclose another stable grant's retained output.
        await expect(
            other.callTool("read_command_output", {
                operation_id: operationId,
            }),
        ).rejects.toThrow("(403)");

        const prepared = await client.callTool<
            JsonObject & { endpoint: string }
        >("prepare_file_download", {
            device_id: testDeviceId,
            path: firstPath,
        });

        const streamResponse = await fetch(
            `${baseUrl}${prepared.result.structuredContent?.endpoint}`,
            { headers: { Authorization: `Bearer ${deviceToken}` } },
        );

        // Direct bearer streaming moves bytes without embedding the full file in MCP JSON.
        expect(streamResponse.status).toBe(200);
        expect(await streamResponse.text()).toBe("alpha MCP marker");

        const uploadedPath = join(agentRoot, "prepared-upload.txt");

        const preparedUpload = await client.callTool<
            JsonObject & { endpoint: string }
        >("prepare_file_upload", {
            device_id: testDeviceId,
            path: uploadedPath,
            on_existing: "error",
        });

        const uploadResponse = await client.stream(
            `${baseUrl}${preparedUpload.result.structuredContent?.endpoint}`,
            { method: "PUT", body: "streamed upload" },
        );

        expect(uploadResponse.status).toBe(200);
        expect(
            (
                await client.callTool("read_file", {
                    device_id: testDeviceId,
                    path: uploadedPath,
                    encoding: "text",
                })
            ).result.structuredContent?.content,
        ).toBe("streamed upload");
        const sameDeviceMoved = join(agentRoot, "prepared-upload-moved.txt");

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
            TEST_SERVER_HOME,
            ".local/share",
            TEST_APP_NAME,
            "mcp/operations/operations.json",
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
                >(
                    "run_command",
                    {
                        device_id: testDeviceId,
                        argv: ["sh", "-lc", "head -c 4096 /dev/zero"],
                        cwd: agentRoot,
                        mode: "foreground",
                        output_limit: 4096,
                    },
                ),
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

        const expiringOperationId =
            quotaOperations.at(-1)?.result.structuredContent?.operation_id;

        if (expiringOperationId === undefined)
            throw new Error("Quota operation missing");
        let expired = false;

        // Every poll spends the credential's request budget, so poll the two-second retention
        // boundary at a modest rate instead of spending most of the per-minute budget on it.
        for (let attempt = 0; attempt < 100 && !expired; attempt += 1) {
            try {
                // Operations are addressable only by their owning credential; the last parallel
                // admission (odd index) belongs to the peer client.
                await peerClient.getOperation(expiringOperationId);
            } catch (error) {
                expired = String(error).includes("(404)");
            }

            if (!expired)
                await new Promise<void>((resolve) => setTimeout(resolve, 100));
        }

        // Terminal metadata and its private spool disappear at the configured retention boundary.
        expect(expired).toBe(true);
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

        const largeFile = join(agentRoot, "large-stream.bin");
        await client.callTool("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", `head -c 1048576 /dev/zero > '${largeFile}'`],
            cwd: agentRoot,
            mode: "foreground",
            output_limit: 128,
        });

        const prepared = await client.callTool<
            JsonObject & { endpoint: string }
        >("prepare_file_download", {
            device_id: testDeviceId,
            path: largeFile,
        });

        const slowResponse = await client.stream(
            `${baseUrl}${prepared.result.structuredContent?.endpoint}`,
        );

        expect(slowResponse.status).toBe(200);
        // A deliberately unconsumed large body must not block independent control traffic.
        expect(
            (await client.callTool("get_device", { device_id: testDeviceId }))
                .result.structuredContent,
        ).toBeDefined();
        await slowResponse.body?.cancel();

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

    it("enforces both transfer endpoints and reauthenticates direct streams", async () => {
        const sourceOnly = await createToken({
            name: "source-only transfer",
            scopes: ["files:read", "files:write", "files:delete"],
            devices: [testDeviceId],
        });

        const destinationOnly = await createToken({
            name: "destination-only transfer",
            scopes: ["files:read", "files:write", "files:delete"],
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

        const sourcePath = join(agentRoot, "prepared-upload-moved.txt");
        const destinationPath = join(secondAgentRoot, "unauthorized-copy.txt");
        // A source grant cannot infer or write a destination outside its device allowlist.
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

        const prepared = await sourceClient.callTool<
            JsonObject & { endpoint: string }
        >("prepare_file_download", {
            device_id: testDeviceId,
            path: sourcePath,
        });

        await tokenCli(["revoke", sourceOnly.credentialId]);

        const rejectedStream = await fetch(
            `${baseUrl}${prepared.result.structuredContent?.endpoint}`,
            { headers: { Authorization: `Bearer ${sourceOnly.token}` } },
        );

        // Streaming requests authenticate afresh, so a prepared URL survives no revocation.
        expect(rejectedStream.status).toBe(401);
    });

    it("retains a changed move source when post-copy cleanup cannot be proven safe", async () => {
        const client = createMcpTestClient({ endpoint, token: deviceToken });
        const sourcePath = join(agentRoot, "cleanup-source.bin");
        const retainedOriginal = join(agentRoot, "cleanup-source-original.bin");

        const destinationPath = join(
            secondAgentRoot,
            "cleanup-destination.bin",
        );

        onTestFinished(() => {
            rmSync(sourcePath, { force: true });
            rmSync(retainedOriginal, { force: true });
            rmSync(destinationPath, { force: true });
        });
        await client.callTool("run_command", {
            device_id: testDeviceId,
            argv: ["sh", "-lc", `head -c 8388608 /dev/zero > '${sourcePath}'`],
            cwd: agentRoot,
            mode: "foreground",
            output_limit: 128,
        });

        const move = await client.callTool<
            JsonObject & { operation_id: string }
        >("move_path", {
            source: { device_id: testDeviceId, path: sourcePath },
            destination: {
                device_id: secondDeviceId,
                path: destinationPath,
            },
            on_existing: "error",
        });

        const operationId = move.result.structuredContent?.operation_id;

        if (operationId === undefined)
            throw new Error("Guarded move operation missing");
        renameSync(sourcePath, retainedOriginal);
        writeFileSync(sourcePath, "replacement must survive");
        const completed = await client.waitForOperation(operationId);
        // Identity-guarded cleanup reports a partial result instead of deleting a replacement path.
        expect(completed.result.structuredContent).toMatchObject({
            state: "failed",
            termination: {
                error: expect.stringContaining("changed"),
            },
        });
        expect(
            (
                await client.callTool("read_file", {
                    device_id: testDeviceId,
                    path: sourcePath,
                    encoding: "text",
                })
            ).result.structuredContent?.content,
        ).toBe("replacement must survive");
        // Destination publication remains visible even though destructive source cleanup failed.
        expect(
            (
                await client.callTool("stat_path", {
                    device_id: secondDeviceId,
                    path: destinationPath,
                })
            ).result.structuredContent,
        ).toMatchObject({ is_file: true });
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
        serverPid = processManager.spawnServer({ config: configPath });
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

    it("requires machine bearer credentials despite the non-API public-path exception", async () => {
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
            headers: { ...headers, Cookie: "redoor_session=fake" },
        });

        // Browser cookies cannot cross the machine-credential boundary.
        expect(cookieOnly.status).toBe(401);

        const publicPage = await fetch(`${baseUrl}/not-an-api-route`);
        // This confirms the neighboring legacy non-API behavior still exists and MCP overrides it.
        expect(publicPage.status).toBe(200);
    });

    it("observes CLI revocation on the next request without a restart", async () => {
        const created = await createToken({ name: "revocation test" });
        const client = createMcpTestClient({ endpoint, token: created.token });
        // A CLI-created credential must be immediately visible to the already-running server.
        expect((await client.callTool("hello")).result.resultType).toBe(
            "complete",
        );
        await tokenCli(["revoke", created.credentialId]);
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

        // Authentication re-reads expiry instead of retaining an accepted in-memory session.
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
        const listed = await tokenCli(["list"]);
        // Administrative metadata intentionally contains neither the one-time token nor verifier fields.
        expect(listed).not.toContain(activeToken);
        expect(listed).not.toContain("verifier");

        const stored = readFileSync(
            join(
                TEST_SERVER_HOME,
                ".local/share",
                TEST_APP_NAME,
                "mcp/machine-tokens.json",
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
