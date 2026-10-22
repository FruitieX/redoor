/** Focuses a mounted filename input and selects its replaceable stem. */
export function focusAndSelectFileNameStem(input: HTMLInputElement | null) {
    if (!input) {
        return;
    }

    const firstDotIndex = input.value.indexOf(".", 1);

    const selectionEnd =
        firstDotIndex === -1 ? input.value.length : firstDotIndex;

    input.focus();
    input.setSelectionRange(0, selectionEnd);
}

/**
 * Suggests a free sibling name for a copy or move whose destination already exists. The
 * counter goes before a file's extension so the renamed copy still opens with the same
 * application, and it skips taken names so the prefilled suggestion is usable as is.
 * Directories get the counter at the end because a dot in their name is not an extension.
 */
export function suggestAvailableFileName(
    name: string,
    entryType: "file" | "directory",
    takenNames: ReadonlySet<string>,
): string {
    // Matches the stem focusAndSelectFileNameStem selects, and keeps dotfiles whole.
    const firstDotIndex = entryType === "file" ? name.indexOf(".", 1) : -1;
    const stem = firstDotIndex === -1 ? name : name.slice(0, firstDotIndex);
    const extension = firstDotIndex === -1 ? "" : name.slice(firstDotIndex);

    for (let counter = 2; ; counter++) {
        const candidate = `${stem}_${counter}${extension}`;

        if (!takenNames.has(candidate)) {
            return candidate;
        }
    }
}
