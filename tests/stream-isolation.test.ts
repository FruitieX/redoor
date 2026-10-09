import {
    afterAll,
    beforeAll,
    describe,
    expect,
    it,
    onTestFinished,
} from "vitest";
import type { Agent, ApiClient } from "#ui/api-client";
import type { TransferProgressEntry } from "#ui/api-client";
import type { ExecEvent } from "#bindings/ExecEvent";
import {
    ProcessManager,
    TempFileManager,
    startServerAndAgent,
    waitForValue,
} from "./test-utils";

const AGENT_NAME = "stream-isolation-agent";

/** Large enough that the unread body cannot hide in socket and HTTP client buffers. */
const STALLED_FILE_BYTES = 64 * 1024 * 1024;

describe("Per-stream transfer isolation", () => {
    const processManager = new ProcessManager();
    const tempFiles = new TempFileManager();
    let apiClient: ApiClient;
    let testAgent: Agent;

    beforeAll(async () => {
        const setup = await startServerAndAgent({
            processManager,
            agentName: AGENT_NAME,
            agentCwd: tempFiles.tempDirectory({ suffix: "-agent-cwd" }),
        });

        apiClient = setup.apiClient;
        testAgent = setup.testAgent;
    }, 30000);

    afterAll(async () => {
        await processManager.killAll();
        tempFiles.cleanup();
    });

    /** Runs one command through the exec endpoint and decodes its NDJSON events. */
    async function execEvents(argv: string[]): Promise<ExecEvent[]> {
        const response = await fetch(
            `${apiClient.baseUrl}/api/v1/agents/${testAgent.id}/exec`,
            {
                method: "POST",
                headers: {
                    ...apiClient.getAuthHeaders(),
                    "Content-Type": "application/json",
                },
                body: JSON.stringify({ argv }),
            },
        );

        // Admission must succeed even though another stream of the agent is stalled.
        expect(response.status).toBe(200);

        return (await response.text())
            .trim()
            .split("\n")
            .map((line): ExecEvent => JSON.parse(line));
    }

    it("keeps other downloads and exec output flowing while one download consumer is stalled", async () => {
        const stalledPath = tempFiles.create(
            Buffer.alloc(STALLED_FILE_BYTES, "s"),
            { suffix: ".bin" },
        );

        const independentContent = "independent download on the same agent";

        const independentPath = tempFiles.create(independentContent, {
            suffix: ".txt",
        });

        const stalledAbort = new AbortController();
        onTestFinished(() => stalledAbort.abort());

        // The response body is deliberately never read, so this consumer stalls once the
        // socket and client buffers are full.
        const stalled = await fetch(testAgent.getRawUrl(stalledPath), {
            headers: testAgent.getAuthHeaders(),
            signal: stalledAbort.signal,
        });

        // Headers arrive only after the agent started streaming the stalled file.
        expect(stalled.status).toBe(200);

        const stalledTransfer = await waitForValue({
            description: "stalled download to transfer its first bytes",
            predicate: async () =>
                (await apiClient.getTransferProgress()).transfers.find(
                    (transfer: TransferProgressEntry) =>
                        transfer.path === stalledPath &&
                        transfer.state === "active" &&
                        transfer.transferred_bytes > 0,
                ),
        });

        // The stalled download really is mid-stream, sharing the agent's transfer socket.
        expect(stalledTransfer.transferred_bytes).toBeLessThan(
            STALLED_FILE_BYTES,
        );

        const independent = Buffer.from(
            await testAgent.raw(independentPath),
        ).toString("utf-8");

        // A second download on the same transfer socket completes despite the stalled consumer.
        expect(independent).toBe(independentContent);

        const events = await execEvents(["sh", "-c", "echo isolated"]);

        // Exec output also shares the transfer socket and must not queue behind the stall.
        expect(
            Buffer.from(
                events
                    .filter((event) => event.type === "stdout")
                    .flatMap((event) => event.data),
            ).toString(),
        ).toBe("isolated\n");
        // The process exit is reported normally, proving the stream reached its terminal frame.
        expect(events.at(-1)).toEqual({ type: "exit", code: 0, signal: null });

        stalledAbort.abort();

        const settled = await waitForValue({
            description: "stalled download to settle after its client left",
            predicate: async () =>
                (await apiClient.getTransferProgress()).transfers.find(
                    (transfer: TransferProgressEntry) =>
                        transfer.path === stalledPath &&
                        transfer.state !== "active",
                ),
        });

        // Dropping the stalled body still cancels its agent worker through the router.
        expect(settled.error).toBe("Download canceled by client");
    }, 60000);
});
