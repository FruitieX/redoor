import * as React from "react";

import {
    isEditorInputTarget,
    isTerminalInputTarget,
    isTextEntryElement,
    isVimEditorTarget,
} from "#ui/utils/keyboard";

/**
 * Lets Escape leave ordinary text controls so keyboard users can return to the shell's
 * shortcuts. Vim editors and terminals keep Escape for themselves, otherwise their modes
 * could not be exited.
 */
export function useEscapeBlursTextControls() {
    React.useEffect(() => {
        /** Blurs the focused text control unless it needs Escape for its own purposes. */
        const handleGlobalFocusKeys = (event: KeyboardEvent) => {
            const activeElement = document.activeElement;

            if (
                event.key === "Escape" &&
                (isTextEntryElement(activeElement) ||
                    isEditorInputTarget(activeElement)) &&
                !isVimEditorTarget(activeElement) &&
                !isVimEditorTarget(event.target) &&
                !isTerminalInputTarget(activeElement) &&
                activeElement instanceof HTMLElement
            ) {
                event.preventDefault();
                activeElement.blur();
            }
        };

        window.addEventListener("keydown", handleGlobalFocusKeys);

        return () =>
            window.removeEventListener("keydown", handleGlobalFocusKeys);
    }, []);
}
