import { afterAll, beforeAll, describe, expect, it, onTestFinished } from "vitest";
import { z } from "zod";

import type { ApiClient } from "#ui/api-client";
import {
    AGENT_PROTOCOL,
    FakeAgent,
    FakeAgentRejectedError,
    upgradeAgentSocket,
} from "./fake-agent";
import {
    ProcessManager,
    TempFileManager,
    startServerAndAgent,
    waitForLogMessage,
    waitForValue,
} from "./test-utils";

const AGENT_NAME = "handshake-agent";

/** Body of a `426 Upgrade Required` answer. */
const protocolMismatchSchema = z.object({
    server_protocol: z.string(),
    server_version: z.string(),
    message: z.string(),
});

describe("agent websocket handshake", () => {
    const processes = new ProcessManager();
    const tempFiles = new TempFileManager();
    let api: ApiClient;
    let serverPid: number;
    let serverPort: number;

    beforeAll(async () => {
        const setup = await startServerAndAgent({
            processManager: processes,
            agentName: AGENT_NAME,
            agentCwd: tempFiles.tempDirectory({ suffix: "-handshake-cwd" }),
        });

        api = setup.apiClient;
        serverPid = setup.serverPid;
        serverPort = setup.serverPort;
    }, 30000);

    afterAll(async () => {
        await processes.killAll();
        tempFiles.cleanup();
    });

    /** Number of sessions the server has started so far, read from its log. */
    function sessionCount(): number {
        return (processes.getStdout(serverPid).match(/Session started/g) ?? [])
            .length;
    }

    /** The server process, for waiting on its log. */
    function serverProcess() {
        const process = processes.getProcess(serverPid);

        if (process === undefined) {
            throw new Error("server process is not tracked");
        }

        return process;
    }

    it("refuses a wrong or missing token before any session exists", async () => {
        const sessionsBefore = sessionCount();
        const agentsBefore = (await api.listAgents()).length;

        for (const token of ["wrong-token", null]) {
            const rejected = waitForLogMessage(
                serverProcess(),
                /Rejecting agent with a missing or invalid token/,
            );

            const outcome = await upgradeAgentSocket({ serverPort, token });

            // The credential is checked during the HTTP upgrade, so no socket is opened.
            expect(outcome).toMatchObject({ kind: "refused", status: 401 });
            await rejected;
        }

        // No session actor was started for either attempt.
        expect(sessionCount()).toBe(sessionsBefore);
        // Nothing unauthenticated can appear in the inventory.
        expect((await api.listAgents()).length).toBe(agentsBefore);
    });

    it("refuses a missing or unknown protocol with 426", async () => {
        for (const protocols of [[], ["redoor.v1"]]) {
            const outcome = await upgradeAgentSocket({
                serverPort,
                protocols,
                version: "0.0.0-old-agent",
            });

            // Incompatible agents fail the upgrade instead of misreading frames.
            expect(outcome.kind).toBe("refused");

            if (outcome.kind !== "refused") {
                return;
            }

            expect(outcome.status).toBe(426);
            const body = protocolMismatchSchema.parse(JSON.parse(outcome.body));
            // The body names the server's protocol so the agent can log what to upgrade.
            expect(body.server_protocol).toBe(AGENT_PROTOCOL);
            expect(body.message).toContain(AGENT_PROTOCOL);
        }

        const mismatches = await api.listProtocolMismatches();
        // The refused agent is remembered for the UI, with its own version.
        expect(
            mismatches.attempts.some(
                (attempt) =>
                    attempt.agent_version === "0.0.0-old-agent" &&
                    attempt.offered_protocols.includes("redoor.v1"),
            ),
        ).toBe(true);
    });

    it("answers the pre-negotiation endpoint with 426", async () => {
        const outcome = await upgradeAgentSocket({
            serverPort,
            path: "/ws",
            token: null,
            protocols: [],
        });

        // Agents of the previous protocol connect here; they must learn to upgrade.
        expect(outcome).toMatchObject({ kind: "refused", status: 426 });
    });

    it("rejects a hello whose name belongs to another live agent", async () => {
        let error: unknown;

        try {
            await FakeAgent.connect({
                serverPort,
                agentId: "handshake-impostor",
                agentName: AGENT_NAME,
            });
        } catch (caught) {
            error = caught;
        }

        // The server answers the hello with `rejected` instead of a takeover.
        expect(error).toBeInstanceOf(FakeAgentRejectedError);
        expect(error).toMatchObject({ reason: "duplicate_name" });
        const agents = await api.listAgents();
        // The real agent keeps its connection.
        expect(
            agents.find((agent) => agent.name === AGENT_NAME)?.status,
        ).toBe("connected");
        // The impostor never appears in the inventory.
        expect(agents.some((agent) => agent.id === "handshake-impostor")).toBe(
            false,
        );
    });

    it("closes a socket that sends frames before hello", async () => {
        const outcome = await upgradeAgentSocket({ serverPort });

        if (outcome.kind !== "open") {
            throw new Error(`upgrade refused with ${outcome.status}`);
        }

        const closed = new Promise<void>((resolve) =>
            outcome.socket.once("close", () => resolve()),
        );

        const logged = waitForLogMessage(
            serverProcess(),
            /Closing agent socket that sent a frame before hello: .*frame=result/,
        );

        outcome.socket.send(
            JSON.stringify({
                type: "result",
                id: 1,
                ok: { message: "forged" },
            }),
        );

        // An unidentified socket must not stay open to retry other frames.
        await closed;
        await logged;
    });

    it("lets a fake agent run commands over the authenticated connection", async () => {
        const fakeName = "handshake-fake-agent";
        const fake = await FakeAgent.connect({ serverPort, agentId: fakeName });
        onTestFinished(() => fake.close());
        fake.onCommand(({ id, command }) => {
            if (command.type === "Echo") {
                fake.respond(id, { message: "from fake" });
            }
        });

        const listed = await waitForValue({
            description: "fake agent connected",
            predicate: async () =>
                (await api.listAgents()).find(
                    (agent) =>
                        agent.name === fakeName && agent.status === "connected",
                ),
        });

        // The welcome's connection id is the one the inventory reports.
        expect(listed.connectionId).toBe(fake.connectionId);
        // Results resolve the REST call although frames carry no agent id.
        expect((await listed.echo("ignored")).message).toBe("from fake");
    });

    it("stops a real agent with a wrong token instead of reconnecting", async () => {
        const agentPid = processes.spawnAgent({
            wsAddress: `ws://127.0.0.1:${serverPort}/api/v1/agent/ws`,
            name: "handshake-wrong-token",
            cwd: tempFiles.tempDirectory({ suffix: "-wrong-token" }),
            token: "not-the-agent-token",
        });

        // A token error cannot fix itself, so the agent exits with a failure status.
        expect(await processes.waitForExit(agentPid)).not.toBe(0);
    });
});
