import React from "react";
import type { CopyExistingMode } from "#bindings/CopyExistingMode";
import type { LsEntry } from "#bindings/LsEntry";
import { validatePathComponent } from "#ui/utils/path";
import { suggestAvailableFileName } from "#ui/utils/file-name";
import { Button } from "#ui/components/button";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { TextField } from "#ui/components/text-field";

/**
 * What the user chooses for an existing destination. Rename is not a server policy: it is
 * sent as the error policy with a new destination name, so if that name got taken in the
 * meantime the transfer still fails instead of overwriting anything. Leaving the existing
 * item alone without transferring is what Cancel is for.
 */
export type ConflictResolution = "rename" | "override" | "merge";

/** Shared conflict choices so Copy, Move, Sync and Upload ask the same destination question. */
const existingItemOptions: Array<{
    value: ConflictResolution;
    label: string;
    description: string;
}> = [
    {
        value: "rename",
        label: "Rename",
        description:
            "Keep the existing item and give the transferred item a new name.",
    },
    {
        value: "override",
        label: "Replace existing",
        description:
            "Replace the entire existing file or directory with the source.",
    },
    {
        value: "merge",
        label: "Merge directories and replace files",
        description: "Replace conflicts but preserve destination-only entries.",
    },
];

/** Maps the choice to the API policy, keeping renamed destinations non-overwriting. */
export function toExistingMode(
    resolution: ConflictResolution,
): CopyExistingMode {
    return resolution === "rename" ? "error" : resolution;
}

/**
 * Falls back to Rename when Merge is no longer offered, for example after the conflicting
 * directory was deselected, so a hidden choice is never submitted.
 */
export function getEffectiveResolution(
    resolution: ConflictResolution,
    canMerge: boolean,
): ConflictResolution {
    return resolution === "merge" && !canMerge ? "rename" : resolution;
}

/** One item that may collide with an existing destination entry of the same name. */
export type ConflictCandidate = {
    key: string;
    fileName: string;
    entryType: "file" | "directory";
};

/** Names already taken in the destination, and which of them could be merged into. */
export function describeDestinationEntries(entries: LsEntry[]) {
    const fileNames = new Set<string>();
    const directoryNames = new Set<string>();

    for (const entry of entries) {
        fileNames.add(entry.name);

        if (entry.type === "directory") {
            directoryNames.add(entry.name);
        }
    }

    return { fileNames, directoryNames };
}

/**
 * Merge only differs from Replace when a directory lands on a directory, so it is offered
 * only when at least one conflict pairs two directories.
 */
export function canMergeConflicts(
    conflicts: ConflictCandidate[],
    destinationDirectoryNames: ReadonlySet<string>,
): boolean {
    return conflicts.some(
        (conflict) =>
            conflict.entryType === "directory" &&
            destinationDirectoryNames.has(conflict.fileName),
    );
}

/**
 * Checks a replacement name before any request starts, so the rename cannot escape the
 * destination directory or land on another item that is already there.
 */
function validateNewName(
    name: string,
    entryType: "file" | "directory",
    takenNames: ReadonlySet<string>,
): string | null {
    const nameError = validatePathComponent(
        name,
        entryType === "file" ? "File" : "Directory",
    );

    if (nameError !== null) {
        return nameError;
    }

    return takenNames.has(name)
        ? `An item named ${name} already exists in the destination`
        : null;
}

/** Renders the conflict choices once so every conflict dialog stays identical. */
export function ExistingItemOptions(props: {
    radioGroupName: string;
    value: ConflictResolution;
    canMerge: boolean;
    onChange: (value: ConflictResolution) => void;
}) {
    const options = props.canMerge
        ? existingItemOptions
        : existingItemOptions.filter((option) => option.value !== "merge");

    return (
        <RadioCardGroup
            legend="Existing item action"
            className="mt-5"
            legendClassName="sr-only"
        >
            {options.map((option) => (
                <RadioCardOption
                    key={option.value}
                    name={props.radioGroupName}
                    value={option.value}
                    label={option.label}
                    description={option.description}
                    checked={props.value === option.value}
                    layout="descriptive"
                    onChange={() => props.onChange(option.value)}
                />
            ))}
        </RadioCardGroup>
    );
}

/** Pairs the new-name input with its validation message so each conflict explains itself. */
function NewNameField(props: {
    label: string;
    originalName: string;
    value: string;
    error: string | null;
    onChange: (value: string) => void;
}) {
    return (
        <div>
            <TextField
                label={props.label}
                value={props.value}
                placeholder={props.originalName}
                description={`The existing ${props.originalName} is kept.`}
                required
                disabled={false}
                onChange={props.onChange}
            />
            {props.error !== null ? (
                <p role="alert" className="mt-1.5 text-xs text-amber-300">
                    {props.error}
                </p>
            ) : null}
        </div>
    );
}

/** Lists one name input per conflict, since each renamed item needs its own name. */
export function NewNameFields(props: {
    renames: PlannedRename[];
    onChange: (key: string, value: string) => void;
}) {
    return (
        <div className="mt-5 grid gap-4">
            {props.renames.map((rename) => (
                <NewNameField
                    key={rename.key}
                    label={`New name for ${rename.item.fileName}`}
                    originalName={rename.item.fileName}
                    value={rename.value}
                    error={rename.error}
                    onChange={(value) => props.onChange(rename.key, value)}
                />
            ))}
        </div>
    );
}

/** One conflicting item with the name it will be transferred under. */
export type PlannedRename = {
    item: ConflictCandidate;
    key: string;
    /** The raw input, so typing whitespace does not jump the caret. */
    value: string;
    name: string;
    error: string | null;
};

/**
 * Returns the keys of items that share a name with an earlier item. Selections can hold one
 * file at two revisions or same-named files from different directories, which would land on
 * one destination path: the first keeps its name and the rest must be renamed, whatever
 * happens to existing destination items.
 */
export function findNameCollisions(
    items: ConflictCandidate[],
): ReadonlySet<string> {
    const seenNames = new Set<string>();

    return new Set(
        items.flatMap((item) => {
            const isRepeat = seenNames.has(item.fileName);

            seenNames.add(item.fileName);

            return isRepeat ? [item.key] : [];
        }),
    );
}

/**
 * Prefills every conflicting item with a distinct free name and validates what the user
 * typed. Items without a conflict keep their own names, so renames must avoid those too,
 * as well as each other.
 */
export function planRenames(props: {
    items: ConflictCandidate[];
    conflicts: ConflictCandidate[];
    destinationFileNames: ReadonlySet<string>;
    editedNames: Record<string, string>;
}): PlannedRename[] {
    const conflictingKeys = new Set(
        props.conflicts.map((conflict) => conflict.key),
    );

    const reservedNames = new Set([
        ...props.destinationFileNames,
        ...props.items
            .filter((item) => !conflictingKeys.has(item.key))
            .map((item) => item.fileName),
    ]);

    const suggestionTakenNames = new Set([
        ...reservedNames,
        ...props.items.map((item) => item.fileName),
    ]);

    const renames = props.conflicts.map((item) => {
        const value =
            props.editedNames[item.key] ??
            suggestAvailableFileName(
                item.fileName,
                item.entryType,
                suggestionTakenNames,
            );

        suggestionTakenNames.add(value.trim());

        return { item, key: item.key, value, name: value.trim() };
    });

    return renames.map((rename, index) => {
        const isDuplicate = renames.some(
            (other, otherIndex) =>
                otherIndex !== index && other.name === rename.name,
        );

        return {
            ...rename,
            error:
                validateNewName(
                    rename.name,
                    rename.item.entryType,
                    reservedNames,
                ) ??
                (isDuplicate
                    ? "Another selected item already uses this name"
                    : null),
        };
    });
}

/**
 * Reuses one Rename / Replace / Merge prompt for the Sync workspace, where a single known
 * destination conflicts. Callers mount it only while a conflict is being resolved, so each
 * prompt starts from the safe default and a fresh suggested name instead of resetting them
 * in an effect.
 */
export function DestinationConflictDialog(props: {
    title: string;
    description: string;
    closeAriaLabel: string;
    confirmLabel: string;
    radioGroupName: string;
    /** The existing destination's name, which the suggested new name is derived from. */
    existingName: string;
    entryType: "file" | "directory";
    /** True only when a directory would land on an existing directory. */
    canMerge: boolean;
    onClose: () => void;
    /** Receives the new destination name when the user chose to rename, otherwise null. */
    onConfirm: (mode: CopyExistingMode, newName: string | null) => void;
}) {
    const [chosenResolution, setResolution] =
        React.useState<ConflictResolution>("rename");

    const resolution = getEffectiveResolution(chosenResolution, props.canMerge);

    const [editedNames, setEditedNames] = React.useState<
        Record<string, string>
    >({});

    const item = {
        key: props.existingName,
        fileName: props.existingName,
        entryType: props.entryType,
    };

    // Sync cannot see the destination's siblings, so only the known conflict is rejected
    // here; the error policy still fails safely if the new name is taken too.
    const renames = planRenames({
        items: [item],
        conflicts: [item],
        destinationFileNames: new Set([props.existingName]),
        editedNames,
    });

    const hasInvalidName =
        resolution === "rename" &&
        renames.some((rename) => rename.error !== null);

    return (
        <Dialog
            isOpen={true}
            title={props.title}
            description={props.description}
            closeAriaLabel={props.closeAriaLabel}
            onClose={props.onClose}
        >
            <ExistingItemOptions
                radioGroupName={props.radioGroupName}
                value={resolution}
                canMerge={props.canMerge}
                onChange={setResolution}
            />
            {resolution === "rename" ? (
                <NewNameFields
                    renames={renames}
                    onChange={(key, value) =>
                        setEditedNames((names) => ({ ...names, [key]: value }))
                    }
                />
            ) : null}
            <DialogActions>
                <Button
                    type="button"
                    variant="secondary"
                    onClick={props.onClose}
                    className="rounded-md text-slate-300"
                >
                    Cancel
                </Button>
                <Button
                    type="button"
                    disabled={hasInvalidName}
                    onClick={() => {
                        props.onClose();
                        props.onConfirm(
                            toExistingMode(resolution),
                            resolution === "rename"
                                ? (renames[0]?.name ?? null)
                                : null,
                        );
                    }}
                    className="rounded-md font-semibold"
                >
                    {props.confirmLabel}
                </Button>
            </DialogActions>
        </Dialog>
    );
}
