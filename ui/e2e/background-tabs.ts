import type { Browser, CDPSession } from "@playwright/test";

/**
 * Follows tabs the browser opens by itself, such as the background tab of a middle-clicked link.
 *
 * `context.waitForEvent("page")` cannot observe those reliably. Playwright reports a new tab only
 * after it receives the tab's first `Page.frameNavigated`, and Chromium drops that event when the
 * tab's document commits while Playwright is still attaching to it. Under CPU load that race is
 * common, so the wait never resolves although the tab opened at the right URL. Browser-level
 * target discovery reports the tab and its URL without depending on that attach.
 */
export class BackgroundTabs {
    /** URLs of the tabs opened since watching began, keyed by target id. */
    private readonly openedTabs = new Map<string, string>();

    private constructor(
        private readonly session: CDPSession,
        private readonly existingTargetIds: Set<string>,
    ) {
        session.on("Target.targetCreated", (event) => {
            this.track(event.targetInfo);
        });
        session.on("Target.targetInfoChanged", (event) => {
            this.track(event.targetInfo);
        });
        session.on("Target.targetDestroyed", (event) => {
            this.openedTabs.delete(event.targetId);
        });
    }

    /**
     * Starts watching just before the action that opens a tab, so pages that already exist,
     * including the test's own page, are never mistaken for the new tab.
     */
    static async watch(browser: Browser) {
        const session = await browser.newBrowserCDPSession();

        // Snapshot before discovery starts, because discovery replays existing targets as created.
        const { targetInfos } = await session.send("Target.getTargets");

        const tabs = new BackgroundTabs(
            session,
            new Set(targetInfos.map((target) => target.targetId)),
        );

        await session.send("Target.setDiscoverTargets", { discover: true });

        return tabs;
    }

    /** Current URLs of the opened tabs, for polling until the expected destination loads. */
    urls() {
        return [...this.openedTabs.values()];
    }

    /** Closes the opened tabs and stops watching so the next test starts from a single page. */
    async close() {
        // Each close emits a destroy event that removes the visited entry, which a Map iteration allows.
        for (const targetId of this.openedTabs.keys()) {
            await this.session.send("Target.closeTarget", { targetId });
        }

        await this.session.detach();
    }

    /** Records new page targets and follows their URL from the blank document to the destination. */
    private track(target: { targetId: string; type: string; url: string }) {
        if (
            target.type !== "page" ||
            this.existingTargetIds.has(target.targetId)
        ) {
            return;
        }

        this.openedTabs.set(target.targetId, target.url);
    }
}
