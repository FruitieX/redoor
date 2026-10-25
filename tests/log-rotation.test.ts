import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { readFileSync, rmSync, statSync, writeFileSync } from "node:fs";

import { type Agent, ApiClient } from "#ui/api-client";
import {
    ProcessManager,
    TempFileManager,
    TEST_AGENT_TOKEN,
    TEST_PASSWORD,
    TEST_USERNAME,
    VITEST_SERVER_PORT,
    waitForPort,
    waitForValue,
} from "./test-utils";

const AGENT_NAME = "log-rotation-agent";

/** Small limit so a few hundred lines are enough to exceed it. */
const LOG_MAX_BYTES = 4 * 1024;

const processManager = new ProcessManager();

const tempFiles = new TempFileManager();

const agentHome = tempFiles.tempDirectory({ suffix: "-log-rotation" });

let apiClient: ApiClient;

let serverLog: string;

let agentLog: string;

/** Builds numbered lines so assertions can tell which half of a log survived. */
function seedLines(count: number): string {
    return Array.from(
        { length: count },
        (_, index) => `seed-line-${index.toString().padStart(4, "0")}\n`,
    ).join("");
}

beforeAll(async () => {
    serverLog = tempFiles.tempFile({ suffix: "-server.log" });
    agentLog = tempFiles.tempFile({ suffix: "-agent.log" });
    rmSync(agentLog, { force: true });
    // Seeded over the limit so the startup check has something to truncate.
    writeFileSync(serverLog, seedLines(1000));

    const configPath = tempFiles.tempFile({ suffix: ".toml" });
    writeFileSync(
        configPath,
        `agent_token = "${TEST_AGENT_TOKEN}"

[server]
username = "${TEST_USERNAME}"
password = "${TEST_PASSWORD}"
log = "${serverLog}"
log_max_size = "${LOG_MAX_BYTES / 1024}KB"

[[agents]]
name = "${AGENT_NAME}"
local = true
home = "${agentHome}"
log = "${agentLog}"
`,
    );
    process.env.REDOOR_PORT = VITEST_SERVER_PORT.toString();
    // No --log flag: the seeded path must come from the config so it is not deleted first.
    processManager.spawnServer({ config: configPath });
    await waitForPort(VITEST_SERVER_PORT);
    apiClient = new ApiClient(`http://127.0.0.1:${VITEST_SERVER_PORT}`);
    await apiClient.login(TEST_USERNAME, TEST_PASSWORD);
});

afterAll(async () => {
    await processManager.killAll();
    tempFiles.cleanup();
});

describe("Log rotation", () => {
    it("truncates an oversized server log to its newer half on startup", () => {
        const log = readFileSync(serverLog, "utf8");

        // The oldest lines are dropped when the log is over its configured limit.
        expect(log).not.toContain("seed-line-0000");
        // The newest seeded history is kept instead of clearing the whole file.
        expect(log).toContain("seed-line-0999");
        // The running server keeps appending to the truncated file.
        expect(log).toContain("Loaded server config");
    });

    it("keeps a managed agent's forwarded log under its limit", async () => {
        const configured = (await apiClient.listAgents()).find(
            (agent) => agent.name === AGENT_NAME,
        );

        if (!configured) throw new Error("Configured local agent missing");
        await configured.start();

        const agent: Agent = await waitForValue({
            description: "managed local agent connection",
            timeoutMs: 30_000,
            predicate: async () =>
                (await apiClient.listAgents()).find(
                    (candidate) =>
                        candidate.name === AGENT_NAME &&
                        candidate.status === "connected",
                ),
        });

        await waitForValue({
            description: "agent startup line in its forwarded log",
            predicate: async () =>
                readFileSync(agentLog, "utf8").includes("Starting agent"),
        });

        // Every command logs a completion line, so these push the log past several
        // hundred-line rotation checks.
        for (let index = 0; index < 300; index++) {
            await agent.ls(agentHome);
        }

        const log = await waitForValue({
            description: "rotated agent log",
            predicate: async () => {
                const content = readFileSync(agentLog, "utf8");

                return content.includes("Starting agent") ? undefined : content;
            },
        });

        // Startup output is the oldest history, so rotation must have dropped it.
        expect(log).not.toContain("Starting agent");
        // Recent command output survives the truncation.
        expect(log).toContain("Command complete");
        // The file stays near its limit: at most one check interval of lines beyond it.
        expect(statSync(agentLog).size).toBeLessThan(LOG_MAX_BYTES + 100 * 512);
    });
});
