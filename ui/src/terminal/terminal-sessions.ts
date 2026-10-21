import type { QueryClient } from "@tanstack/react-query";
import type { createStore } from "jotai";

import type { Agent } from "#ui/api-client";
import {
    activateBottomDrawerTabAtom,
    bottomDrawerActiveTabAtom,
    bottomDrawerCollapsedAtom,
} from "#ui/bottom-drawer-state";
import { queryKeys } from "#ui/queries";
import { OneShotTerminalCommand } from "#ui/terminal/one-shot-command";
import {
    TerminalSession,
    type BrowserListingRefreshTarget,
    type TerminalTabSnapshot,
} from "#ui/terminal/terminal-session";
import { ExternalStore } from "#ui/utils/external-store";
import { moveKeyedItems, type ItemMove } from "#ui/utils/reorder";

/** Identifies the routed agent and directory used by the direct new-terminal action. */
export type ActiveTerminalTarget = { agent: Agent; cwd: string };

/** Adds one-shot shell input only for feature-created terminals. */
export type TerminalCreationTarget = ActiveTerminalTarget & {
    startupCommand?: string;
    refreshTarget?: BrowserListingRefreshTarget;
};

/** What the terminal panel renders: the tabs and the few facts about how they are shown. */
export type TerminalSessionsSnapshot = {
    readonly tabs: readonly TerminalTabSnapshot[];
    readonly activeTabId: number | null;
    /** Whether the device picker is open, because the shortcut opens it when no target exists. */
    readonly isPickerOpen: boolean;
    /** Counts focus requests so the view can retry focusing once its DOM is interactive. */
    readonly focusEpoch: number;
    /** Whether the drawer is expanded on the terminal pane. */
    readonly isPanelVisible: boolean;
};

/** Delays the refresh so a command that was just typed has time to change the directory. */
const LISTING_REFRESH_DELAY_MS = 1000;

/** Reuses a live shell only when it was opened in the same directory as the current browse path. */
function findOpenTerminalForTarget(
    sessions: readonly TerminalSession[],
    activeTabId: number | null,
    target: ActiveTerminalTarget,
): TerminalSession | undefined {
    const matching = sessions.filter(
        (session) =>
            session.tab.agent.id === target.agent.id &&
            session.tab.cwd === target.cwd &&
            session.tab.state.type !== "disconnected",
    );

    return (
        matching.find((session) => session.tab.id === activeTabId) ??
        matching.at(-1)
    );
}

/**
 * Owns every terminal tab of the application. Tabs and their shells outlive routes, the
 * drawer and any component, so the owner is created once with the application and everything
 * else asks it to do things: features call `create` to run a command in a new shell, the
 * shortcut and the toolbar button call `openOrFocus`, and the panel only subscribes to the
 * snapshot. The drawer's visibility is read from the shared store, which is also how a tab
 * learns whether it is being shown.
 */
export class TerminalSessions extends ExternalStore<TerminalSessionsSnapshot> {
    private sessions: TerminalSession[] = [];
    private activeTabId: number | null = null;
    private isPickerOpen = false;
    private focusEpoch = 0;
    private isPanelVisible = false;
    private pendingFocus = false;
    private theme: "dark" | "light" = "dark";
    private nextTabId = 1;
    private readonly nextNumberByAgent = new Map<string, number>();
    private readonly refreshTimers = new Set<number>();
    private readonly unsubscribeDrawer: () => void;

    /** Needs the query client for listing refreshes and the store for the drawer atoms. */
    constructor(
        private readonly queryClient: QueryClient,
        private readonly store: ReturnType<typeof createStore>,
    ) {
        super({
            tabs: [],
            activeTabId: null,
            isPickerOpen: false,
            focusEpoch: 0,
            isPanelVisible: false,
        });

        const unsubscribeCollapsed = store.sub(bottomDrawerCollapsedAtom, () =>
            this.syncVisibility(),
        );

        const unsubscribeTab = store.sub(bottomDrawerActiveTabAtom, () =>
            this.syncVisibility(),
        );

        this.unsubscribeDrawer = () => {
            unsubscribeCollapsed();
            unsubscribeTab();
        };

        this.isPanelVisible = this.readPanelVisible();
    }

    /** Opens a fresh shell for a target, such as one that runs an extraction command. */
    create(target: TerminalCreationTarget): void {
        const id = this.nextTabId;
        this.nextTabId += 1;

        const agentTerminalNumber =
            this.nextNumberByAgent.get(target.agent.id) ?? 1;

        this.nextNumberByAgent.set(target.agent.id, agentTerminalNumber + 1);
        const refreshTarget = target.refreshTarget;

        const startupCommand =
            target.startupCommand !== undefined && refreshTarget !== undefined
                ? new OneShotTerminalCommand(target.startupCommand, () =>
                      this.scheduleListingRefresh(refreshTarget),
                  )
                : null;

        const session = new TerminalSession({
            id,
            agent: target.agent,
            agentTerminalNumber,
            title: `${target.agent.name} ${agentTerminalNumber}`,
            cwd: target.cwd,
            theme: this.theme,
            startupCommand,
            onChange: () => this.emit(),
        });

        this.sessions = [...this.sessions, session];
        this.activeTabId = id;
        this.revealTerminal();
        this.requestFocus();
    }

    /**
     * Shares tab reuse and focus between the shortcut and the toolbar button: reuse the live
     * shell for the same directory, create one if there is none, or open the device picker
     * when there is no routed target.
     */
    openOrFocus(target: ActiveTerminalTarget | null): void {
        if (target === null) {
            if (!this.isPickerOpen) {
                this.revealTerminal();
                this.setPickerOpen(true);
            }

            return;
        }

        const existing = findOpenTerminalForTarget(
            this.sessions,
            this.activeTabId,
            target,
        );

        if (existing) {
            this.activeTabId = existing.tab.id;
            this.revealTerminal();
            this.requestFocus();
        } else {
            this.create(target);
        }
    }

    /** Opens or closes the device picker, which the panel's menu reflects. */
    setPickerOpen(isOpen: boolean): void {
        this.isPickerOpen = isOpen;
        this.emit();
    }

    /** Makes a tab the visible one. */
    select(tabId: number): void {
        if (this.activeTabId === tabId) {
            return;
        }

        this.activeTabId = tabId;
        this.syncVisibility();
    }

    /** Disposes one shell and picks its nearest surviving neighbour when it was active. */
    close(tabId: number): void {
        const index = this.sessions.findIndex(
            (session) => session.tab.id === tabId,
        );

        const session = this.sessions[index];

        if (session === undefined) {
            return;
        }

        session.dispose();
        this.sessions = this.sessions.filter((entry) => entry !== session);

        if (this.activeTabId === tabId) {
            const replacement =
                this.sessions[index] ?? this.sessions[index - 1] ?? null;

            this.activeTabId = replacement?.tab.id ?? null;
        }

        this.syncVisibility();
    }

    /** Starts a new shell in a disconnected tab while preserving its identity and directory. */
    restart(tabId: number): void {
        const session = this.sessions.find((entry) => entry.tab.id === tabId);

        if (session === undefined) {
            return;
        }

        this.activeTabId = tabId;
        session.restart();
        this.revealTerminal();
        this.requestFocus();
    }

    /** Reorders tab presentation without replacing any live shell. */
    move(move: ItemMove): void {
        this.sessions = moveKeyedItems(
            this.sessions,
            (session) => String(session.tab.id),
            move,
        );

        this.emit();
    }

    /** Gives a session to its view, which attaches the session's host element. */
    sessionFor(tabId: number): TerminalSession | undefined {
        return this.sessions.find((session) => session.tab.id === tabId);
    }

    /** Follows the app theme for every shell and for those created later. */
    setTheme(theme: "dark" | "light"): void {
        this.theme = theme;

        for (const session of this.sessions) {
            session.setTheme(theme);
        }
    }

    /** Drops a pending focus request, because the user already went back to the editor. */
    cancelPendingFocus(): void {
        this.pendingFocus = false;
    }

    /**
     * Moves focus into the active shell once it can take it: displayed and connected. The
     * view calls this after each render that could have made that true, because a tab that
     * was just revealed is still inert until React has committed it. A late connection after
     * the user returned to the editor must not keep the shell, so the editor takes focus back.
     */
    flushFocus(tabId: number): void {
        const session = this.sessions.find((entry) => entry.tab.id === tabId);

        if (
            this.focusEpoch === 0 ||
            session === undefined ||
            this.activeTabId !== tabId ||
            !this.isPanelVisible ||
            session.tab.state.type !== "connected"
        ) {
            return;
        }

        if (this.pendingFocus) {
            session.focus();
            this.pendingFocus = false;

            return;
        }

        const editor = document.querySelector<HTMLElement>(
            '[role="textbox"][data-file-editor]',
        );

        if (
            editor &&
            document.activeElement?.closest("[data-terminal-input]")
        ) {
            editor.focus();
        }
    }

    /** Releases every shell, timer and drawer subscription, for tests and teardown. */
    dispose(): void {
        this.unsubscribeDrawer();

        for (const timer of this.refreshTimers) {
            window.clearTimeout(timer);
        }

        this.refreshTimers.clear();

        for (const session of this.sessions) {
            session.dispose();
        }

        this.sessions = [];
        this.activeTabId = null;
        this.emit();
    }

    /** Expands the drawer onto the terminal pane; the drawer's atoms are the only state. */
    private revealTerminal(): void {
        this.store.set(activateBottomDrawerTabAtom, "terminal");
        this.syncVisibility();
    }

    /** Remembers that the user asked for the shell, to focus it as soon as it is ready. */
    private requestFocus(): void {
        this.pendingFocus = true;
        this.focusEpoch += 1;
        this.emit();
    }

    /** Reads whether the drawer shows the terminal pane at all. */
    private readPanelVisible(): boolean {
        return (
            !this.store.get(bottomDrawerCollapsedAtom) &&
            this.store.get(bottomDrawerActiveTabAtom) === "terminal"
        );
    }

    /** Tells each session whether it is on screen, which gates starting and resizing. */
    private syncVisibility(): void {
        this.isPanelVisible = this.readPanelVisible();

        for (const session of this.sessions) {
            session.setShown(
                this.isPanelVisible && session.tab.id === this.activeTabId,
            );
        }

        this.emit();
    }

    /**
     * Invalidates only the listing that originated a command, a moment after it was typed.
     * The timer outlives the tab, because the command keeps running after the tab closes.
     */
    private scheduleListingRefresh(target: BrowserListingRefreshTarget): void {
        const timer = window.setTimeout(() => {
            this.refreshTimers.delete(timer);

            void this.queryClient.invalidateQueries({
                queryKey: queryKeys.browserListing(target.agentId, target.path),
                exact: true,
                refetchType: "active",
            });
        }, LISTING_REFRESH_DELAY_MS);

        this.refreshTimers.add(timer);
    }

    /** Publishes a snapshot that reuses the arrays and tab objects that did not change. */
    private emit(): void {
        const previous = this.getSnapshot();
        const tabs = this.sessions.map((session) => session.tab);

        const tabsUnchanged =
            tabs.length === previous.tabs.length &&
            tabs.every((tab, index) => tab === previous.tabs[index]);

        const next: TerminalSessionsSnapshot = {
            tabs: tabsUnchanged ? previous.tabs : tabs,
            activeTabId: this.activeTabId,
            isPickerOpen: this.isPickerOpen,
            focusEpoch: this.focusEpoch,
            isPanelVisible: this.isPanelVisible,
        };

        if (
            next.tabs === previous.tabs &&
            next.activeTabId === previous.activeTabId &&
            next.isPickerOpen === previous.isPickerOpen &&
            next.focusEpoch === previous.focusEpoch &&
            next.isPanelVisible === previous.isPanelVisible
        ) {
            return;
        }

        this.publish(next);
    }
}
