import { copyFileSync, chmodSync, mkdirSync, statSync } from "node:fs";
import { join } from "node:path";
import {
    afterAll,
    beforeAll,
    describe,
    expect,
    it,
    onTestFinished,
} from "vitest";
import WebSocket from "ws";
import { z } from "zod";

import type { AgentDetailsResponse } from "#bindings/AgentDetailsResponse";
import { ApiError, type Agent, type ApiClient } from "#ui/api-client";
import {
    ProcessManager,
    SERVER_PATH,
    TEST_APP_NAME,
    TEST_SERVER_HOME,
    TempFileManager,
    startServerAndAgent,
    waitForValue,
    webSocketDataToString,
} from "./test-utils";

const AGENT_NAME = "upgrade-external-agent";

const transferSocketOpenSchema = z.object({
    type: z.literal("transfer_socket_open"),
    token: z.string().min(1),
});

const controlMessageSchema = z.object({ type: z.string() });

/** Agent results the early-completion fixture answers with, in their wire shape. */
type FakeCommandResult =
    | { type: "RawUpload" }
    | ({ type: "GetAgentDetails" } & AgentDetailsResponse);

/** Waits for a websocket to become writable without relying on timing delays. */
async function waitForSocketOpen(socket: WebSocket): Promise<void> {
    await new Promise<void>((resolve, reject) => {
        socket.once("open", resolve);
        socket.once("error", reject);
    });
}

/** Reads one JSON control message from a websocket fixture. */
async function nextTransferSocketOpenMessage(
    socket: WebSocket,
): Promise<z.infer<typeof transferSocketOpenSchema>> {
    return new Promise((resolve, reject) => {
        socket.once("message", (data) => {
            try {
                resolve(
                    transferSocketOpenSchema.parse(
                        JSON.parse(webSocketDataToString(data)),
                    ),
                );
            } catch (error) {
                reject(error);
            }
        });
        socket.once("error", reject);
    });
}

describe("connected external agent upgrade", () => {
    const processes = new ProcessManager();
    const tempFiles = new TempFileManager();
    let api: ApiClient;
    let agent: Agent;
    let agentPid: number;
    let serverPort: number;
    let executablePath: string;
    let targetVersion: string;

    beforeAll(async () => {
        executablePath = tempFiles.tempFile({ suffix: "-redoor" });
        copyFileSync(SERVER_PATH, executablePath);
        chmodSync(executablePath, 0o751);

        const setup = await startServerAndAgent({
            processManager: processes,
            agentName: AGENT_NAME,
            agentCwd: tempFiles.tempDirectory({ suffix: "-cwd" }),
            agentExecutablePath: executablePath,
        });

        api = setup.apiClient;
        agent = setup.testAgent;
        agentPid = setup.agentPid;
        serverPort = setup.serverPort;
        const serverInfo = await api.getServerInfo();
        const details = await agent.getDetails();
        targetVersion = serverInfo.version;

        // Seed the namespaced XDG cache the server uses so upgrades never hit GitHub.
        const cachedBinary = join(
            TEST_SERVER_HOME,
            ".cache",
            TEST_APP_NAME,
            "binaries",
            targetVersion,
            details.os,
            details.arch,
            "redoor",
        );

        mkdirSync(join(cachedBinary, ".."), { recursive: true });
        copyFileSync(SERVER_PATH, cachedBinary);
        chmodSync(cachedBinary, 0o755);
    }, 30000);

    afterAll(async () => {
        await processes.killAll();
        tempFiles.cleanup();
    });

    it("rejects an invalid target version before replacing the agent", async () => {
        let error: unknown;

        try {
            await agent.upgrade("../invalid");
        } catch (caught) {
            error = caught;
        }

        // Invalid versions must not become release URL or cache path components.
        expect(error).toBeInstanceOf(ApiError);
        // A client error lets the UI report the bad manual value without implying an outage.
        expect(error).toMatchObject({
            status: 400,
            message: "Invalid Redoor release version: ../invalid",
        });
        // Rejection before upload must leave the original connection serving commands.
        expect((await agent.echo("upgrade-not-started")).message).toBe(
            "upgrade-not-started",
        );
    });

    it("atomically replaces and self-execs the connected external agent", async () => {
        const originalConnectionId = agent.connectionId;
        expect(originalConnectionId).not.toBeNull();

        const response = await agent.upgrade(targetVersion);
        // The endpoint acknowledges only after replacement and self-exec setup succeed.
        expect(response).toEqual({
            upgrading: true,
            target_version: targetVersion,
        });

        const upgraded = await waitForValue({
            timeoutMs: 30000,
            description: "upgraded agent replacement connection",
            predicate: async () => {
                const candidate = (await api.listAgents()).find(
                    (entry) => entry.name === AGENT_NAME,
                );

                return candidate?.status === "connected" &&
                    candidate.connectionId !== originalConnectionId &&
                    candidate.binary?.version === targetVersion
                    ? candidate
                    : undefined;
            },
        });

        const details = await upgraded.getDetails();
        // Self-exec must preserve the process identity instead of relying on a supervisor respawn.
        expect(details.pid).toBe(agentPid);
        // The replacement must report the manually selected target version.
        expect(details.binary.version).toBe(targetVersion);
        // Atomic executable replacement must retain all pre-existing mode bits.
        expect(statSync(executablePath).mode & 0o777).toBe(0o751);
        // A lightweight control command proves the replacement connection serves requests.
        expect((await upgraded.echo("upgrade-ready")).message).toBe(
            "upgrade-ready",
        );
    }, 60000);

    it("force-installs the exact running server binary", async () => {
        const serverInfo = await api.getServerInfo();

        const currentAgent = (await api.listAgents()).find(
            (entry) =>
                entry.name === AGENT_NAME && entry.status === "connected",
        );

        // A connected generation is required to prove force installation reconnects it.
        expect(currentAgent?.connectionId).not.toBeNull();
        expect(currentAgent).toBeDefined();

        const response = await agent.forceInstallRunningBinary();
        // The force endpoint reports the version baked into the running server executable.
        expect(response).toEqual({
            upgrading: true,
            target_version: serverInfo.version,
        });

        const upgraded = await waitForValue({
            timeoutMs: 30000,
            description: "force-installed agent replacement connection",
            predicate: async () => {
                const candidate = (await api.listAgents()).find(
                    (entry) => entry.name === AGENT_NAME,
                );

                return candidate?.status === "connected" &&
                    candidate.connectionId !== currentAgent?.connectionId
                    ? candidate
                    : undefined;
            },
        });

        const details = await upgraded.getDetails();
        // Exact identity equality proves the running image was used instead of a release cache.
        expect(details.binary).toEqual({
            version: serverInfo.version,
            git_rev: serverInfo.git_rev,
            git_dirty: serverInfo.git_dirty,
            version_dirty: serverInfo.version_dirty,
            build_mode: serverInfo.build_mode,
            build_date: serverInfo.build_date,
        });
        // Self-exec must preserve PID for the force path as well.
        expect(details.pid).toBe(agentPid);
    }, 60000);

    it("rejects an older agent before sending an upload command", async () => {
        const oldAgentName = "upgrade-unsupported-agent";
        const serverInfo = await api.getServerInfo();
        const control = new WebSocket(`ws://127.0.0.1:${serverPort}/ws`);
        let transfer: WebSocket | undefined;
        onTestFinished(() => {
            control.close();
            transfer?.close();
        });
        await waitForSocketOpen(control);
        control.send(
            JSON.stringify({
                type: "agent_register",
                agent_id: oldAgentName,
                agent_name: oldAgentName,
                os: "linux",
                arch: "x86_64",
                hostname: "old-host",
                username: "old-user",
                cwd: "/tmp",
                token: serverInfo.agent_token,
                binary: {
                    version: "0.0.3",
                    git_rev: "old",
                    git_dirty: false,
                    version_dirty: false,
                    build_mode: "release",
                    build_date: "unknown",
                },
                // Deliberately omit supports_self_exec to model a pre-protocol agent.
            }),
        );
        const transferOpen = await nextTransferSocketOpenMessage(control);
        // The transfer socket must be opened before the fixture can authenticate it.
        expect(transferOpen.type).toBe("transfer_socket_open");
        // A non-empty one-time token binds the transfer socket to this control connection.
        expect(transferOpen.token.length).toBeGreaterThan(0);

        transfer = new WebSocket(
            `ws://127.0.0.1:${serverPort}/api/v1/agent-transfer/ws`,
        );
        await waitForSocketOpen(transfer);
        transfer.send(
            JSON.stringify({
                type: "authenticate",
                agent_id: oldAgentName,
                token: transferOpen.token,
            }),
        );

        const oldAgent = await waitForValue({
            description: "unsupported fixture transfer registration",
            predicate: async () => {
                const candidate = (await api.listAgents()).find(
                    (entry) => entry.name === oldAgentName,
                );

                return candidate?.status === "connected"
                    ? candidate
                    : undefined;
            },
        });

        // An omitted capability must remain visible as unsupported metadata.
        expect(oldAgent.supportsSelfExec).toBe(false);

        const receivedCommandTypes: string[] = [];
        control.on("message", (data) => {
            receivedCommandTypes.push(
                controlMessageSchema.parse(
                    JSON.parse(webSocketDataToString(data)),
                ).type,
            );
        });
        let error: unknown;

        try {
            await oldAgent.upgrade(targetVersion);
        } catch (caught) {
            error = caught;
        }

        // Conflict plus remediation tells operators how to make the upgrade safe.
        expect(error).toBeInstanceOf(ApiError);
        expect(error).toMatchObject({
            status: 409,
            message: expect.stringContaining(
                "Install a current Redoor agent manually",
            ),
        });
        // No RawUpload command proves rejection happened before any remote mutation began.
        expect(receivedCommandTypes).toEqual([]);
    });

    it("refuses to self-exec when the upgrade upload completes before any byte", async () => {
        const earlyAgentName = "upgrade-early-completion-agent";
        const serverInfo = await api.getServerInfo();
        const arch = process.arch === "arm64" ? "aarch64" : "x86_64";
        const control = new WebSocket(`ws://127.0.0.1:${serverPort}/ws`);
        let transfer: WebSocket | undefined;
        onTestFinished(() => {
            control.close();
            transfer?.close();
        });
        await waitForSocketOpen(control);
        control.send(
            JSON.stringify({
                type: "agent_register",
                agent_id: earlyAgentName,
                agent_name: earlyAgentName,
                os: "linux",
                arch,
                hostname: "early-host",
                username: "early-user",
                cwd: "/tmp",
                token: serverInfo.agent_token,
                binary: {
                    version: serverInfo.version,
                    git_rev: "early",
                    git_dirty: false,
                    version_dirty: false,
                    build_mode: "debug",
                    build_date: "unknown",
                },
                supports_self_exec: true,
            }),
        );
        const transferOpen = await nextTransferSocketOpenMessage(control);

        transfer = new WebSocket(
            `ws://127.0.0.1:${serverPort}/api/v1/agent-transfer/ws`,
        );
        await waitForSocketOpen(transfer);
        transfer.send(
            JSON.stringify({
                type: "authenticate",
                agent_id: earlyAgentName,
                token: transferOpen.token,
            }),
        );

        const earlyAgent = await waitForValue({
            description: "early-completion fixture transfer registration",
            predicate: async () => {
                const candidate = (await api.listAgents()).find(
                    (entry) => entry.name === earlyAgentName,
                );

                return candidate?.status === "connected"
                    ? candidate
                    : undefined;
            },
        });

        const commandSchema = z.object({
            type: z.literal("command"),
            request_id: z.number(),
            command: z.object({ type: z.string() }),
        });

        const receivedCommandTypes: string[] = [];
        control.on("message", (data) => {
            const parsed = commandSchema.safeParse(
                JSON.parse(webSocketDataToString(data)),
            );

            if (!parsed.success) {
                return;
            }

            const { request_id: requestId, command } = parsed.data;
            receivedCommandTypes.push(command.type);

            const respond = (result: FakeCommandResult): void => {
                control.send(
                    JSON.stringify({
                        type: "command_response",
                        agent_id: earlyAgentName,
                        request_id: requestId,
                        result,
                    }),
                );
            };

            if (command.type === "GetAgentDetails") {
                respond({
                    type: "GetAgentDetails",
                    id: earlyAgentName,
                    name: earlyAgentName,
                    pid: 1,
                    cwd: "/tmp",
                    config_path: "",
                    exe_path: "/tmp/early-redoor",
                    load_average_one: 0,
                    load_average_five: 0,
                    load_average_fifteen: 0,
                    system_uptime: 0,
                    os: "linux",
                    arch,
                    hostname: "early-host",
                    external_ip: null,
                    username: "early-user",
                    connected_at: 0,
                    binary: {
                        version: serverInfo.version,
                        git_rev: "early",
                        git_dirty: false,
                        version_dirty: false,
                        build_mode: "debug",
                        build_date: "unknown",
                    },
                    mount_points: [],
                    uid: 1000,
                    is_root: false,
                });
            } else if (command.type === "RawUpload") {
                // Finishing before transfer_ready means the agent never received the binary.
                respond({ type: "RawUpload" });
            }
        });

        let error: unknown;

        try {
            await earlyAgent.forceInstallRunningBinary();
        } catch (caught) {
            error = caught;
        }

        // An upload that stored no bytes must fail instead of reporting a started upgrade.
        expect(error).toBeInstanceOf(ApiError);
        expect(error).toMatchObject({
            status: 500,
            message: "Unexpected early upgrade upload completion",
        });
        // The upload was attempted, but self-exec must never follow it.
        expect(receivedCommandTypes).toContain("RawUpload");
        expect(receivedCommandTypes).not.toContain("SelfExec");
    });
});
