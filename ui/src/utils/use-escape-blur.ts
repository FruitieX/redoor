import { shortcutPriority, useShortcut } from "#ui/shortcuts";
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
    useShortcut({
        name: "escape-blurs-text-controls",
        // Bubble, so a control that used Escape itself, such as a menu, keeps it.
        phase: "bubble",
        priority: shortcutPriority.app,
        match: (event) =>
            event.key === "Escape" &&
            (isTextEntryElement(document.activeElement) ||
                isEditorInputTarget(document.activeElement)) &&
            !isVimEditorTarget(document.activeElement) &&
            !isVimEditorTarget(event.target) &&
            !isTerminalInputTarget(document.activeElement),
        run: () => {
            const activeElement = document.activeElement;

            if (!(activeElement instanceof HTMLElement)) {
                return false;
            }

            activeElement.blur();
        },
    });
}
