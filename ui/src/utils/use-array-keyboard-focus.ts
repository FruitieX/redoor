import { isPlainKey } from "#ui/utils/keyboard";
import { shortcutPriority, useShortcut } from "#ui/shortcuts";

/**
 * Moves focus through the current ordered elements returned by a component. A modal list
 * passes a higher priority than the page behind it, otherwise the page's own list, which
 * registered first, would claim the keys while the dialog covers it.
 */
export function useArrayKeyboardFocus(
    getElements: () => HTMLElement[],
    enabled = true,
    priority: number = shortcutPriority.view,
) {
    /** Advances without wrapping so repeated keys remain at list boundaries. */
    const moveFocus = (event: KeyboardEvent) => {
        const elements = getElements();

        if (elements.length === 0) {
            return false;
        }

        const focusedIndex = elements.findIndex(
            (element) => element === document.activeElement,
        );

        const nextIndex =
            event.key === "j"
                ? Math.min(focusedIndex + 1, elements.length - 1)
                : focusedIndex === -1
                  ? elements.length - 1
                  : Math.max(focusedIndex - 1, 0);

        elements[nextIndex]?.focus();
    };

    useShortcut(
        {
            name: "array-focus-next-previous",
            phase: "bubble",
            priority,
            skipTypingTargets: true,
            match: (event) => isPlainKey(event, "j") || isPlainKey(event, "k"),
            run: moveFocus,
        },
        enabled,
    );
}
