import * as React from "react";

import { shortcutPriority, useShortcut } from "#ui/shortcuts";

/** Matches the controls a keyboard user can reach with Tab, which is what a trap must cycle. */
export const focusableSelector =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Keeps Tab inside a modal container by wrapping focus at either end. Every overlay that
 * covers the page without the browser's own modal behaviour (the side-menu drawer, anchored
 * menus) needs the same rule, so it lives here instead of being re-derived per overlay.
 *
 * The trap is a modal-tier shortcut binding, so it takes part in the same ordering as the
 * overlays' Escape handling. It only claims the key at the first or last control; anywhere
 * else the browser's own Tab order does the work, and a Tab from outside the container is
 * left alone so stacked overlays do not fight over it. A container with nothing focusable
 * keeps focus on itself, so give it `tabIndex={-1}`.
 */
export function useFocusTrap(
    containerRef: React.RefObject<HTMLElement | null>,
    isActive: boolean,
) {
    useShortcut(
        {
            name: "focus-trap",
            phase: "bubble",
            priority: shortcutPriority.modal,
            match: (event) => event.key === "Tab",
            run: (event) => {
                const container = containerRef.current;

                if (!container) {
                    return false;
                }

                const entries = [
                    ...container.querySelectorAll<HTMLElement>(
                        focusableSelector,
                    ),
                ];

                const first = entries[0];
                const last = entries.at(-1);

                if (!first || !last) {
                    container.focus();

                    return;
                }

                if (event.shiftKey && document.activeElement === first) {
                    last.focus();
                } else if (!event.shiftKey && document.activeElement === last) {
                    first.focus();
                } else {
                    return false;
                }
            },
        },
        isActive,
    );
}
