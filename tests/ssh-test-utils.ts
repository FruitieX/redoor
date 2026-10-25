import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

/** SSH alias of the disposable test host the SSH integration tests provision. */
export const SSH_TEST_HOST = "redoor-ssh-test";

/** Unprivileged key-authenticated account on the SSH test host. */
export const SSH_TEST_USER = "redoor";

/** Quotes one controlled value for the remote POSIX shell used by OpenSSH. */
export function shellQuote(value: string): string {
    return `'${value.replaceAll("'", `'"'"'`)}'`;
}

/** Runs a command on the SSH test host as the required redoor user. */
export async function sshTestCommand(command: string): Promise<void> {
    await execFileAsync("ssh", ["-l", SSH_TEST_USER, SSH_TEST_HOST, command]);
}

/** Stops any orphaned remote agent and removes only this test's isolated files. */
export async function cleanupSshTest(options: {
    remoteRoot: string;
    agentAppNames: string[];
    sshCommand?: (command: string) => Promise<void>;
}): Promise<void> {
    const run = options.sshCommand ?? sshTestCommand;
    const appNames = options.agentAppNames.map(shellQuote).join(" ");
    const command = `for app in ${appNames}; do pid_file="$HOME/.local/share/$app/agent.pid"; if [ -f "$pid_file" ]; then pid=$(tr -d '[:space:]' < "$pid_file"); case "$pid" in *[!0-9]*|'') ;; *) kill "$pid" 2>/dev/null || true ;; esac; fi; rm -rf "$HOME/.local/share/$app"; done; rm -rf ${shellQuote(options.remoteRoot)}`;
    await run(command);
}
