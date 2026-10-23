/**
 * Turns any thrown value into text for the user. Failures reach the UI as `Error`s from the
 * API client, but a promise can reject with anything, so every surface shares this one
 * fallback instead of repeating the `instanceof` check.
 */
export function getErrorMessage(cause: unknown, fallbackMessage: string) {
    if (cause instanceof Error) {
        return cause.message;
    }

    return fallbackMessage;
}
