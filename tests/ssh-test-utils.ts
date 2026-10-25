import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { $ } from "zx";

/** SSH alias of the disposable test host the SSH integration tests provision. */
export const SSH_TEST_HOST = "redoor-ssh-test";

/** Unprivileged key-authenticated account on the SSH test host. */
export const SSH_TEST_USER = "redoor";

/** Quotes one controlled value for the remote POSIX shell used by OpenSSH. */
export function shellQuote(value: string): string {
    return `'${value.replaceAll("'", `'"'"'`)}'`;
}

/** Password-only account on the SSH test host, for tests that cover password login. */
export const SSH_PASSWORD_USER = "redoor-password";

/**
 * Runs a command on the SSH test host as the required redoor user. Returns stdout so tests
 * can inspect remote state such as `authorized_keys`.
 */
export async function sshTestCommand(command: string): Promise<string> {
    const { stdout } = await $`ssh -l ${SSH_TEST_USER} ${SSH_TEST_HOST} ${command}`.quiet();

    return stdout;
}

/**
 * Runs a command as a password-only account using the shared fixture password, without
 * exposing it in process arguments or test output. Returns stdout.
 */
export async function sshCommandWithPassword(
    username: string,
    command: string,
): Promise<string> {
    const password = process.env.REDOOR_SSH_TEST_PASSWORD;

    if (password === undefined || password === "") {
        throw new Error(
            "REDOOR_SSH_TEST_PASSWORD is required for password SSH commands",
        );
    }

    const askpassDir = mkdtempSync(join(tmpdir(), "redoor-ssh-askpass-"));
    const askpassPath = join(askpassDir, "askpass");
    writeFileSync(
        askpassPath,
        `#!/bin/sh
printf '%s' "$REDOOR_SSH_TEST_PASSWORD"
`,
        { mode: 0o700 },
    );

    try {
        const { stdout } = await $({
            env: {
                ...process.env,
                DISPLAY: process.env.DISPLAY ?? ":0",
                SSH_ASKPASS: askpassPath,
                SSH_ASKPASS_REQUIRE: "force",
            },
        })`ssh -l ${username} ${SSH_TEST_HOST} ${command}`.quiet();

        return stdout;
    } finally {
        rmSync(askpassDir, { recursive: true, force: true });
    }
}

/** Stops any orphaned remote agent and removes only this test's isolated files. */
export async function cleanupSshTest(options: {
    remoteRoot: string;
    agentAppNames: string[];
    sshCommand?: (command: string) => Promise<string>;
}): Promise<void> {
    const run = options.sshCommand ?? sshTestCommand;
    const appNames = options.agentAppNames.map(shellQuote).join(" ");
    const command = `for app in ${appNames}; do pid_file="$HOME/.local/share/$app/agent.pid"; if [ -f "$pid_file" ]; then pid=$(tr -d '[:space:]' < "$pid_file"); case "$pid" in *[!0-9]*|'') ;; *) kill "$pid" 2>/dev/null || true ;; esac; fi; rm -rf "$HOME/.local/share/$app"; done; rm -rf ${shellQuote(options.remoteRoot)}`;
    await run(command);
}
