/**
 * Orders devices by name, then id, so equal names still land in a stable order. Every device
 * list shares this so a device never jumps between menus and pages.
 */
export function sortAgentsByName<T extends { id: string; name: string }>(
    agents: readonly T[],
): T[] {
    return [...agents].sort(
        (left, right) =>
            left.name.localeCompare(right.name) ||
            left.id.localeCompare(right.id),
    );
}
