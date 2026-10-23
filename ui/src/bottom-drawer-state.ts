import { atom } from "jotai";

/** Identifies one primary pane in the shared application drawer. */
export type BottomDrawerTabId = "selected" | "transfers" | "terminal";

/** Shares drawer visibility so feature buttons can fold an already visible pane. */
export const bottomDrawerCollapsedAtom = atom(true);

/** Identifies which drawer pane a feature toggle would hide. */
export const bottomDrawerActiveTabAtom = atom<BottomDrawerTabId>("terminal");

/**
 * Reveals a pane after a direct user action by changing the tab and expanding the drawer in
 * one write. The drawer reads the same two atoms, so there is no request to queue and no
 * effect to drain it: the action is the state change.
 */
export const activateBottomDrawerTabAtom = atom(
    null,
    (_get, set, tab: BottomDrawerTabId) => {
        set(bottomDrawerActiveTabAtom, tab);
        set(bottomDrawerCollapsedAtom, false);
    },
);
