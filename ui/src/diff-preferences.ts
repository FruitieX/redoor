import { atomWithLocalStorage } from "#ui/utils/local-storage-atom";

/**
 * Whether diffs pair a similar deleted and inserted line into one row with inline changes,
 * or list every removed and added line separately. It is one browser-wide preference, so
 * every diff reads the same way and the choice survives reloads. Inline rows are the
 * default because most edits change a few words of a line.
 */
export const inlineDiffChangesAtom = atomWithLocalStorage<boolean>(
    "redoor.diff-inline-changes",
    true,
);
