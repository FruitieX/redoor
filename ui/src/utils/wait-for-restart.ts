/** Polls a target-specific probe until it proves the replacement process is ready. */
export async function waitForRestart(
    probe: () => Promise<void>,
    timeoutMessage: string,
): Promise<void> {
    const startedAt = Date.now();

    while (Date.now() - startedAt < 30_000) {
        try {
            await probe();

            return;
        } catch {
            // The target is still shutting down or starting up.
        }

        await new Promise((resolve) => setTimeout(resolve, 100));
    }

    throw new Error(timeoutMessage);
}
