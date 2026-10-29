import { randomBytes } from "node:crypto";
import {
    appendFileSync,
    chmodSync,
    copyFileSync,
    mkdirSync,
    mkdtempSync,
    rmSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, onTestFinished, test } from "vitest";
import { ApiClient } from "#ui/api-client";
import {
    ProcessManager,
    SERVER_PATH,
    TEST_AGENT_TOKEN,
    TEST_PASSWORD,
    TEST_USERNAME,
    VITEST_SERVER_PORT,
    waitForPort,
    waitForValue,
} from "./test-utils";
import {
    SSH_TEST_HOST,
    SSH_TEST_USER,
    cleanupSshTest,
    shellQuote,
    sshTestCommand,
} from "./ssh-test-utils";

/** Status line prefix the installer reports right before streaming a binary to the device. */
const UPLOADING = "Uploading the binary to ";

/** Status line prefix the installer reports when the device already has the binary. */
const REUSING = "Using existing remote binary at ";

/** Matches the content-addressed debug install path and captures the build hash. */
const DEBUG_INSTALL = /\/binaries\/debug-([0-9a-f]{16})\/redoor$/;

/** Lists the install directories of `appName` on the SSH test host, sorted. */
async function remoteInstalls(appName: string): Promise<string[]> {
    const output = await sshTestCommand(
        `cd "$HOME/.local/share" && ls -1 ${shellQuote(appName)}/binaries`,
    );

    return output
        .split("\n")
        .filter((line) => line !== "")
        .sort();
}

/** Returns the build hash named by the one status line that starts with `prefix`. */
function installedHash(messages: string[], prefix: string): string {
    const lines = messages.filter((message) => message.startsWith(prefix));
    expect(lines).toHaveLength(1);
    const hash = DEBUG_INSTALL.exec(lines[0] ?? "")?.[1];

    if (hash === undefined) {
        throw new Error(`No debug install path in: ${lines.join(", ")}`);
    }

    return hash;
}

// The server only uploads its debug build to a device of its own platform, and the SSH
// test host is x86_64 Linux.
describe.skipIf(
    process.env.REDOOR_SSH_TEST !== "1" ||
        process.platform !== "linux" ||
        process.arch !== "x64",
)("SSH debug build provisioning", () => {
    test("reuses an unchanged debug build and replaces a rebuilt one", async () => {
        const processManager = new ProcessManager();
        const home = mkdtempSync(join(tmpdir(), "redoor-ssh-debug-build-"));
        const suffix = `${process.pid}-${Date.now()}`;
        const appName = `redoor-ssh-debug-build-${suffix}`;
        const agentName = `ssh-debug-build-${suffix}`;
        const remoteRoot = `/tmp/redoor-ssh-debug-build-${suffix}`;
        const configPath = join(home, "config.toml");

        // A private copy stands in for `target/debug/redoor`, so the test can "rebuild" it
        // without touching the shared build output other suites run.
        const debugBuild = join(home, "build", "redoor");
        mkdirSync(join(home, "build"));
        copyFileSync(SERVER_PATH, debugBuild);
        chmodSync(debugBuild, 0o755);

        writeFileSync(
            configPath,
            `agent_token = "${TEST_AGENT_TOKEN}"

[server]
username = "${TEST_USERNAME}"
password = "${TEST_PASSWORD}"
port = ${VITEST_SERVER_PORT}

[[agents]]
target = "${SSH_TEST_HOST}"
username = "${SSH_TEST_USER}"
name = "${agentName}"
home = "${remoteRoot}"
log = "${join(home, "managed-agent.log")}"
`,
        );

        onTestFinished(async () => {
            await processManager.killAll();

            try {
                await cleanupSshTest({ remoteRoot, agentAppNames: [appName] });
            } finally {
                rmSync(home, { recursive: true, force: true });
            }
        });

        await cleanupSshTest({ remoteRoot, agentAppNames: [appName] });
        // Installs left by older servers: the legacy fixed debug path, another build's
        // content-addressed path, and a versioned release install that must survive.
        await sshTestCommand(
            `mkdir -p ${shellQuote(remoteRoot)} && cd "$HOME/.local/share" && mkdir -p ${shellQuote(appName)}/binaries/debug ${shellQuote(appName)}/binaries/debug-0000000000000000 ${shellQuote(appName)}/binaries/1.2.3`,
        );

        const serverPid = processManager.spawn(
            SERVER_PATH,
            ["server", "--config", configPath],
            {
                env: {
                    ...process.env,
                    HOME: home,
                    REDOOR_APP_NAME: appName,
                    REDOOR_TEST_DEBUG_BINARY: debugBuild,
                },
            },
        );

        await waitForPort(VITEST_SERVER_PORT);
        const apiClient = new ApiClient(`http://127.0.0.1:${VITEST_SERVER_PORT}`);
        await apiClient.login(TEST_USERNAME, TEST_PASSWORD);

        /**
         * Starts the stopped agent, waits for a new connection and returns every
         * provisioning step seen on the way, then stops it again for the next start.
         */
        async function startOnce(): Promise<string[]> {
            const stopped = await waitForValue({
                predicate: async () =>
                    (await apiClient.listAgents()).find(
                        (agent) =>
                            agent.name === agentName && agent.status === "stopped",
                    ),
                description: "stopped SSH debug build agent",
            });

            await stopped.start();

            const seen = new Set<string>();

            const connected = await waitForValue({
                predicate: async () => {
                    const agent = (await apiClient.listAgents()).find(
                        (entry) => entry.name === agentName,
                    );

                    for (const step of agent?.provisioningStatus ?? []) {
                        seen.add(step.message);
                    }

                    if (agent?.connectionId) {
                        return agent;
                    }

                    throw new Error(
                        `status=${agent?.status ?? "missing"}, issue=${agent?.connectionIssue ?? "none"}, server=${processManager.getStdout(serverPid)}`,
                    );
                },
                timeoutMs: 90_000,
                description: "SSH debug build agent to connect",
            });

            for (const step of connected.provisioningStatus) {
                seen.add(step.message);
            }

            const shutdown = await connected.shutdown();
            // The next start must begin from a stopped agent, not race the old session.
            expect(shutdown.agent.status).toBe("stopped");

            return [...seen];
        }

        const first = await startOnce();
        // A fresh device has no install of this build, so it must be uploaded.
        const firstHash = installedHash(first, UPLOADING);
        // The verified upload removes the legacy and other-build debug installs and keeps
        // the release install. The cleanup may finish after the agent connects, so poll.
        await waitForValue({
            predicate: async () => {
                const installs = await remoteInstalls(appName);
                expect(installs).toEqual(["1.2.3", `debug-${firstHash}`]);

                return installs;
            },
            description: "stale debug installs to be removed",
        });

        const second = await startOnce();
        // The unchanged build is found at its content-addressed path and not uploaded again.
        expect(second.filter((message) => message.startsWith(UPLOADING))).toEqual(
            [],
        );
        expect(installedHash(second, REUSING)).toBe(firstHash);

        // Extra trailing bytes change the content hash and size but still run, like a
        // rebuild that keeps the version.
        appendFileSync(debugBuild, randomBytes(4096));

        const third = await startOnce();
        const rebuiltHash = installedHash(third, UPLOADING);
        // The rebuilt binary gets its own directory instead of overwriting the old one.
        expect(rebuiltHash).not.toBe(firstHash);
        // Only the rebuilt install remains next to the release install.
        await waitForValue({
            predicate: async () => {
                const installs = await remoteInstalls(appName);
                expect(installs).toEqual(["1.2.3", `debug-${rebuiltHash}`]);

                return installs;
            },
            description: "previous debug build install to be removed",
        });
    }, 300_000);
});
