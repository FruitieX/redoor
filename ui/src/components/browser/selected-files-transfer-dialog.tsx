import React from "react";
import { useQuery } from "@tanstack/react-query";
import type { ApiClient } from "#ui/api-client";
import type { ResumeMode } from "#bindings/ResumeMode";
import { formatSize } from "#ui/utils/path";
import { useSetAtom } from "jotai";
import { Copy, FolderInput } from "lucide-react";
import type { Agent } from "#ui/api-client";
import type { CopyExistingMode } from "#bindings/CopyExistingMode";
import { Button } from "#ui/components/button";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { joinBrowserPath } from "#ui/components/browser/utils";
import {
    getSelectedFileKey,
    type SelectedPath,
    unselectFileAtom,
} from "#ui/selected-files";
import { AffectedPathsList } from "#ui/components/browser/affected-paths-list";
import {
    canMergeConflicts,
    type ConflictCandidate,
    describeDestinationEntries,
    type ConflictResolution,
    ExistingItemOptions,
    getEffectiveResolution,
    NewNameFields,
    planRenames,
    toExistingMode,
} from "#ui/components/browser/destination-conflict";
import type { LsEntry } from "#bindings/LsEntry";

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
    /** The destination listing, whose entry types decide whether Merge is offered. */
    destinationEntries: LsEntry[];
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

    const [chosenResolution, setResolution] =
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

    const destination = describeDestinationEntries(props.destinationEntries);

    const candidates: ConflictCandidate[] = props.selectedFiles.map((file) => ({
        key: getSelectedFileKey(file),
        fileName: file.fileName,
        entryType: file.entryType,
    }));

    const conflictingFiles = props.selectedFiles.filter((file) => {
        const destinationPath = joinBrowserPath(
            props.directoryPath,
            file.fileName,
        );

        const isSamePath =
            file.agentId === props.destinationAgent.id &&
            file.path === destinationPath;

        return !isSamePath && destination.fileNames.has(file.fileName);
    });

    const conflictingKeys = new Set(
        conflictingFiles.map((file) => getSelectedFileKey(file)),
    );

    const conflicts = candidates.filter((candidate) =>
        conflictingKeys.has(candidate.key),
    );

    const canMerge = canMergeConflicts(conflicts, destination.directoryNames);
    const resolution = getEffectiveResolution(chosenResolution, canMerge);

    const renames = planRenames({
        items: candidates,
        conflicts,
        destinationFileNames: destination.fileNames,
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
                        canMerge={canMerge}
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
