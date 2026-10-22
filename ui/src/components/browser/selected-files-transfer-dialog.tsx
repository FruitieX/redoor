import React from "react";
import { useQuery } from "@tanstack/react-query";
import type { ApiClient } from "#ui/api-client";
import type { ResumeMode } from "#bindings/ResumeMode";
import { formatSize, validatePathComponent } from "#ui/utils/path";
import { suggestAvailableFileName } from "#ui/utils/file-name";
import { useSetAtom } from "jotai";
import { Copy, FolderInput } from "lucide-react";
import type { Agent } from "#ui/api-client";
import type { CopyExistingMode } from "#bindings/CopyExistingMode";
import { Button } from "#ui/components/button";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { TextField } from "#ui/components/text-field";
import { joinBrowserPath } from "#ui/components/browser/utils";
import {
    getSelectedFileKey,
    type SelectedPath,
    unselectFileAtom,
} from "#ui/selected-files";
import { AffectedPathsList } from "#ui/components/browser/affected-paths-list";

/**
 * What the user chooses for an existing destination. Rename is not a server policy: it is
 * sent as the error policy with a new destination name, so if that name got taken in the
 * meantime the transfer still fails instead of overwriting anything. Leaving the existing
 * item alone without transferring is what Cancel is for.
 */
type ConflictResolution = "rename" | "override" | "merge";

/** Shared conflict choices so Copy, Move and Sync ask the same destination question. */
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
function toExistingMode(resolution: ConflictResolution): CopyExistingMode {
    return resolution === "rename" ? "error" : resolution;
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

/** Renders the conflict choices once so both dialogs stay identical. */
function ExistingItemOptions(props: {
    radioGroupName: string;
    value: ConflictResolution;
    onChange: (value: ConflictResolution) => void;
}) {
    return (
        <RadioCardGroup
            legend="Existing item action"
            className="mt-5"
            legendClassName="sr-only"
        >
            {existingItemOptions.map((option) => (
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
                description={`Replaces ${props.originalName} in the destination.`}
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
function NewNameFields(props: {
    renames: PlannedRename[];
    onChange: (key: string, value: string) => void;
}) {
    return (
        <div className="mt-5 grid gap-4">
            {props.renames.map((rename) => (
                <NewNameField
                    key={rename.key}
                    label={`New name for ${rename.file.fileName}`}
                    originalName={rename.file.fileName}
                    value={rename.value}
                    error={rename.error}
                    onChange={(value) => props.onChange(rename.key, value)}
                />
            ))}
        </div>
    );
}

/** One conflicting selection with the name it will be transferred under. */
type PlannedRename = {
    file: SelectedPath;
    key: string;
    /** The raw input, so typing whitespace does not jump the caret. */
    value: string;
    name: string;
    error: string | null;
};

/**
 * Prefills every conflicting selection with a distinct free name and validates what the user
 * typed. Selections without a conflict keep their own names, so renames must avoid those too,
 * as well as each other.
 */
function planRenames(props: {
    selectedFiles: SelectedPath[];
    conflictingFiles: SelectedPath[];
    destinationFileNames: ReadonlySet<string>;
    editedNames: Record<string, string>;
}): PlannedRename[] {
    const conflictingKeys = new Set(
        props.conflictingFiles.map((file) => getSelectedFileKey(file)),
    );

    const reservedNames = new Set([
        ...props.destinationFileNames,
        ...props.selectedFiles
            .filter((file) => !conflictingKeys.has(getSelectedFileKey(file)))
            .map((file) => file.fileName),
    ]);

    const suggestionTakenNames = new Set([
        ...reservedNames,
        ...props.selectedFiles.map((file) => file.fileName),
    ]);

    const renames = props.conflictingFiles.map((file) => {
        const key = getSelectedFileKey(file);

        const value =
            props.editedNames[key] ??
            suggestAvailableFileName(
                file.fileName,
                file.entryType,
                suggestionTakenNames,
            );

        suggestionTakenNames.add(value.trim());

        return { file, key, value, name: value.trim() };
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
                    rename.file.entryType,
                    reservedNames,
                ) ??
                (isDuplicate
                    ? "Another selected item already uses this name"
                    : null),
        };
    });
}

/**
 * Operation-specific chrome lives here so Copy keeps its existing accessible
 * names while Move can reuse the same conflict gate.
 */
const transferTriggerCopy = {
    buttonLabel: "Copy",
    buttonTooltip: "Copy selected items to this directory",
    buttonAriaLabel: "Copy selected items to this directory",
    closeAriaLabel: "Close copy conflict dialog",
    confirmLabel: "Continue copying",
    radioGroupName: "copy-on-existing",
    Icon: Copy,
};

const transferTriggerMove = {
    buttonLabel: "Move",
    buttonTooltip: "Move selected items to this directory",
    buttonAriaLabel: "Move selected items to this directory",
    closeAriaLabel: "Close move conflict dialog",
    confirmLabel: "Continue moving",
    radioGroupName: "move-on-existing",
    Icon: FolderInput,
};

export type SelectedFilesTransferOperation = "copy" | "move";

/** Shared by listing buttons and the compact mobile menu so both start the same run. */
export type SelectedFilesTransferTriggerApi = {
    canTransfer: boolean;
    start: () => void;
    labels: typeof transferTriggerCopy;
};

/**
 * Reuses one Rename / Replace / Merge prompt for listing transfers and the Sync workspace.
 * Callers mount it only while a conflict is being resolved, so each prompt starts from the
 * safe default and a fresh suggested name instead of resetting them in an effect.
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
    onClose: () => void;
    /** Receives the new destination name when the user chose to rename, otherwise null. */
    onConfirm: (mode: CopyExistingMode, newName: string | null) => void;
}) {
    const [resolution, setResolution] =
        React.useState<ConflictResolution>("rename");

    const [newName, setNewName] = React.useState(() =>
        suggestAvailableFileName(
            props.existingName,
            props.entryType,
            new Set([props.existingName]),
        ),
    );

    const trimmedName = newName.trim();

    // Sync cannot see the destination's siblings, so only the known conflict is rejected
    // here; the error policy still fails safely if the new name is taken too.
    const nameError =
        resolution === "rename"
            ? validateNewName(
                  trimmedName,
                  props.entryType,
                  new Set([props.existingName]),
              )
            : null;

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
                onChange={setResolution}
            />
            {resolution === "rename" ? (
                <div className="mt-5">
                    <NewNameField
                        label={`New name for ${props.existingName}`}
                        originalName={props.existingName}
                        value={newName}
                        error={nameError}
                        onChange={setNewName}
                    />
                </div>
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
                    disabled={nameError !== null}
                    onClick={() => {
                        props.onClose();
                        props.onConfirm(
                            toExistingMode(resolution),
                            resolution === "rename" ? trimmedName : null,
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

/** Offers resuming an interrupted transfer, independently of the conflict choice. */
function ResumeOptions(props: {
    candidate: { partial_bytes: number; total_bytes: number };
    value: ResumeMode;
    onChange: (value: ResumeMode) => void;
}) {
    return (
        <RadioCardGroup
            legend={`Interrupted transfer: ${formatSize(props.candidate.partial_bytes)} / ${formatSize(props.candidate.total_bytes)}`}
            className="mt-5"
        >
            <RadioCardOption
                layout="descriptive"
                name="transfer-resume"
                value="auto"
                label="Resume"
                description="Verify and continue the partial file."
                checked={props.value === "auto"}
                onChange={() => props.onChange("auto")}
            />
            <RadioCardOption
                layout="descriptive"
                name="transfer-resume"
                value="restart"
                label="Start over"
                description="Discard partial progress and copy from the beginning."
                checked={props.value === "restart"}
                onChange={() => props.onChange("restart")}
            />
        </RadioCardGroup>
    );
}

/** Always confirms selected paths and includes a conflict policy when needed. */
export function SelectedFilesTransferTrigger(props: {
    api: ApiClient;
    operation: SelectedFilesTransferOperation;
    selectedFiles: SelectedPath[];
    destinationAgent: Agent;
    directoryPath: string;
    destinationFileNames: string[];
    canTransfer: boolean;
    /** Renamed destinations are keyed by `getSelectedFileKey`; others keep their names. */
    onConfirm: (
        mode: CopyExistingMode,
        resume: ResumeMode,
        destinationNames: ReadonlyMap<string, string>,
    ) => void;
    children: (trigger: SelectedFilesTransferTriggerApi) => React.ReactNode;
}) {
    const [isOpen, setIsOpen] = React.useState(false);

    const [resolution, setResolution] =
        React.useState<ConflictResolution>("rename");

    // Only names the user edited are stored; the rest follow the current suggestion.
    const [editedNames, setEditedNames] = React.useState<
        Record<string, string>
    >({});

    const [resumeMode, setResumeMode] = React.useState<ResumeMode>("auto");

    const preflight = useQuery({
        queryKey: [
            "copy-preflight",
            props.destinationAgent.id,
            props.directoryPath,
            props.selectedFiles,
        ],
        enabled: isOpen,
        queryFn: async () =>
            Promise.all(
                props.selectedFiles.map((file) =>
                    props.api.copyPreflight({
                        source: { agent: file.agentId, path: file.path },
                        dest: {
                            agent: props.destinationAgent.id,
                            path: joinBrowserPath(
                                props.directoryPath,
                                file.fileName,
                            ),
                        },
                    }),
                ),
            ),
    });

    const candidate = preflight.data?.find(
        (result) => result.resume_candidate,
    )?.resume_candidate;

    const unselectFile = useSetAtom(unselectFileAtom);
    const copy = props.operation === "copy";
    const labels = copy ? transferTriggerCopy : transferTriggerMove;
    const destinationFileNames = new Set(props.destinationFileNames);

    const conflictingFiles = props.selectedFiles.filter((file) => {
        const destinationPath = joinBrowserPath(
            props.directoryPath,
            file.fileName,
        );

        const isSamePath =
            file.agentId === props.destinationAgent.id &&
            file.path === destinationPath;

        return !isSamePath && destinationFileNames.has(file.fileName);
    });

    const renames = planRenames({
        selectedFiles: props.selectedFiles,
        conflictingFiles,
        destinationFileNames,
        editedNames,
    });

    /** Opens one confirmation surface for both ordinary and conflicting transfers. */
    const handleStart = () => {
        setResolution("rename");
        setEditedNames({});
        setResumeMode("auto");
        setIsOpen(true);
    };

    const operationLabel = copy ? "Copy" : "Move";
    const hasConflicts = conflictingFiles.length > 0;
    const isRenaming = hasConflicts && resolution === "rename";

    const hasInvalidName =
        isRenaming && renames.some((rename) => rename.error !== null);

    // Clearing the selection dismisses the prompt. Resetting the state while rendering, rather
    // than hiding the dialog, keeps it from reappearing when something is selected again.
    if (isOpen && props.selectedFiles.length === 0) {
        setIsOpen(false);
    }

    return (
        <>
            {props.children({
                canTransfer: props.canTransfer,
                start: handleStart,
                labels,
            })}
            <Dialog
                isOpen={isOpen}
                size="wide"
                title={
                    hasConflicts
                        ? "Destination items already exist"
                        : `${operationLabel} selected ${props.selectedFiles.length === 1 ? "item" : "items"}?`
                }
                description={
                    hasConflicts
                        ? `${conflictingFiles.length} selected ${conflictingFiles.length === 1 ? "item has" : "items have"} the same name as an item in this directory. Choose how to handle the existing destination.`
                        : `${operationLabel} the selected ${props.selectedFiles.length === 1 ? "item" : "items"} to ${props.destinationAgent.name}:${props.directoryPath}.`
                }
                closeAriaLabel={labels.closeAriaLabel}
                onClose={() => setIsOpen(false)}
            >
                <AffectedPathsList
                    paths={props.selectedFiles}
                    onDeselect={(path) =>
                        unselectFile({ agentId: path.agentId, path: path.path })
                    }
                />
                {candidate ? (
                    <ResumeOptions
                        candidate={candidate}
                        value={resumeMode}
                        onChange={setResumeMode}
                    />
                ) : null}
                {preflight.error ? (
                    <p role="alert" className="mt-5 text-sm text-amber-300">
                        Cannot check for an interrupted transfer:{" "}
                        {preflight.error.message}
                    </p>
                ) : null}
                {hasConflicts ? (
                    <ExistingItemOptions
                        radioGroupName={labels.radioGroupName}
                        value={resolution}
                        onChange={setResolution}
                    />
                ) : null}
                {isRenaming ? (
                    <NewNameFields
                        renames={renames}
                        onChange={(key, value) =>
                            setEditedNames((names) => ({
                                ...names,
                                [key]: value,
                            }))
                        }
                    />
                ) : null}
                <DialogActions>
                    <Button
                        type="button"
                        variant="secondary"
                        onClick={() => setIsOpen(false)}
                        className="rounded-md text-slate-300"
                    >
                        Cancel
                    </Button>
                    <Button
                        type="button"
                        // A failed check must not block the copy; the server still resumes
                        // a valid partial because the default mode is auto.
                        disabled={preflight.isPending || hasInvalidName}
                        onClick={() => {
                            setIsOpen(false);
                            props.onConfirm(
                                hasConflicts
                                    ? toExistingMode(resolution)
                                    : "error",
                                resumeMode,
                                new Map(
                                    isRenaming
                                        ? renames.map((rename) => [
                                              rename.key,
                                              rename.name,
                                          ])
                                        : [],
                                ),
                            );
                        }}
                        className="rounded-md font-semibold"
                    >
                        {hasConflicts
                            ? labels.confirmLabel
                            : `${operationLabel} selected ${props.selectedFiles.length === 1 ? "item" : "items"}`}
                    </Button>
                </DialogActions>
            </Dialog>
        </>
    );
}
