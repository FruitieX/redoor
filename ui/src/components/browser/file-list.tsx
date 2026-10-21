import React from "react";
import { atom, useAtom, useAtomValue, useSetAtom } from "jotai";
import { Link, useNavigate } from "@tanstack/react-router";
import {
    ArrowDownUp,
    ChevronDown,
    ChevronUp,
    Download,
    File,
    Folder,
    Search,
} from "lucide-react";
import type { LsEntry } from "#bindings/LsEntry";
import type { MountPoint } from "#bindings/MountPoint";
import type { Agent } from "#ui/api-client";
import { Button } from "#ui/components/button";
import { Checkbox } from "#ui/components/checkbox";
import { PathActionMenu } from "#ui/components/browser/path-actions";
import { DeletePathDialog } from "#ui/components/browser/delete-path-dialog";
import { InputControl } from "#ui/components/input-control";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { TextField } from "#ui/components/text-field";
import { Tooltip } from "#ui/components/tooltip";
import {
    selectedFileKeysAtom,
    toggleSelectedFileAtom,
} from "#ui/selected-files";
import {
    compareFileEntries,
    type FileSort,
    formatModifiedAge,
    joinBrowserPath,
} from "#ui/components/browser/utils";
import type { FileSortColumn } from "#ui/browser-search";
import { browserRoute } from "#ui/browser-route";
import { formatSize } from "#ui/utils/path";
import { shortcutPriority, useShortcut } from "#ui/shortcuts";
import { isPlainKey } from "#ui/utils/keyboard";
import { useArrayKeyboardFocus } from "#ui/utils/use-array-keyboard-focus";
import {
    buildUnarchiveCommand,
    getArchiveInfo,
    getDirectoryArchiveName,
    getCustomArchiveDirectoryError,
    type UnarchiveDestination,
} from "#ui/utils/archive";
import { useTerminals } from "#ui/use-terminals";

/** Identifies the destination that should restore filter focus after Enter navigation. */
const filterFocusPathAtom = atom<string | null>(null);

/** Handles shortcuts that only exist while the file list is mounted. */
function useFileListShortcuts(props: {
    filterInputRef: React.RefObject<HTMLInputElement | null>;
    filter: string;
    setFilter: React.Dispatch<React.SetStateAction<string>>;
}) {
    // Both keys stay out of text controls, so typing an f or leaving the filter box with
    // Escape never triggers them; Escape clears the filter only once focus is elsewhere.
    useShortcut({
        name: "file-list-focus-filter",
        phase: "bubble",
        priority: shortcutPriority.view,
        skipTypingTargets: true,
        match: (event) => isPlainKey(event, "f"),
        run: () => {
            props.filterInputRef.current?.focus();
        },
    });

    useShortcut({
        name: "file-list-clear-filter",
        phase: "bubble",
        priority: shortcutPriority.view,
        skipTypingTargets: true,
        match: (event) => isPlainKey(event, "Escape"),
        run: () => {
            if (props.filter === "") {
                return false;
            }

            props.setFilter("");
        },
    });
}

/** Filters only the loaded directory entries so recursive search remains in the shared dialog. */
export function FileList(props: {
    agent: Agent;
    directoryPath: string;
    actions: React.ReactNode;
    files: LsEntry[];
    showHiddenFiles: boolean;
    mountPoint: MountPoint | null;
    /** Comes from the URL so the order survives a reload; null keeps the listing order. */
    sort: FileSort | null;
    onSort: (column: FileSortColumn) => void;
}) {
    const navigate = useNavigate();
    const agent = props.agent;
    const [filterFocusPath, setFilterFocusPath] = useAtom(filterFocusPathAtom);
    const filterInputRef = React.useRef<HTMLInputElement>(null);
    const [filter, setFilter] = React.useState("");

    const normalizedFilter = filter.toLowerCase();
    // A leading dot is a hidden-name query, so include those entries without flipping the toggle.
    const includeHiddenFiles = props.showHiddenFiles || filter.startsWith(".");

    const visibleFiles = includeHiddenFiles
        ? props.files
        : props.files.filter((entry) => !entry.name.startsWith("."));

    const filteredFiles = visibleFiles.filter((entry) =>
        entry.name.toLowerCase().includes(normalizedFilter),
    );

    const sort = props.sort;

    const displayedFiles = sort
        ? [...filteredFiles].sort((left, right) => {
              return compareFileEntries(left, right, sort);
          })
        : filteredFiles;

    useFileListShortcuts({
        filterInputRef,
        filter,
        setFilter,
    });

    React.useEffect(() => {
        if (filterFocusPath !== props.directoryPath) {
            return;
        }

        filterInputRef.current?.focus();
        setFilterFocusPath(null);
    }, [filterFocusPath, props.directoryPath, setFilterFocusPath]);

    const handleFilterKeyDown = async (
        event: React.KeyboardEvent<HTMLInputElement>,
    ) => {
        if (event.key !== "Enter") {
            return;
        }

        const destinationPath = filteredFiles[0]
            ? joinBrowserPath(props.directoryPath, filteredFiles[0].name)
            : undefined;

        if (!destinationPath) {
            return;
        }

        event.preventDefault();
        setFilterFocusPath(destinationPath);
        await navigate(browserRoute(agent.id, destinationPath));
    };

    return (
        <div className="overflow-hidden bg-surface">
            {props.actions}
            <div className="flex flex-wrap items-center gap-1.5 border-b border-slate-800 bg-slate-900/35 p-1.5 sm:gap-2 sm:p-2">
                <label className="relative min-w-0 flex-1">
                    <span className="sr-only">Filter files</span>
                    <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-500" />
                    <InputControl
                        ref={filterInputRef}
                        type="search"
                        aria-label="Filter files"
                        value={filter}
                        onChange={(event) => setFilter(event.target.value)}
                        onKeyDown={handleFilterKeyDown}
                        placeholder="Filter files (f)"
                        className="w-full bg-slate-900 py-1.5 pl-9 text-sm placeholder:text-slate-500 focus:ring-1 focus:ring-blue-500 sm:py-2"
                    />
                </label>
            </div>
            <FileTable
                agent={props.agent}
                directoryPath={props.directoryPath}
                files={displayedFiles}
                sort={sort}
                onSort={props.onSort}
            />
            <FilesystemFooter mountPoint={props.mountPoint} />
        </div>
    );
}

/** Keeps current filesystem capacity visible below local file results. */
function FilesystemFooter(props: { mountPoint: MountPoint | null }) {
    const mountPoint = props.mountPoint;
    const availableBytes = mountPoint?.available_bytes ?? null;
    const totalBytes = mountPoint?.total_bytes ?? null;

    const usedBytes =
        totalBytes !== null && availableBytes !== null
            ? Math.max(0, totalBytes - availableBytes)
            : null;

    const usage =
        usedBytes === null || availableBytes === null
            ? "Unavailable"
            : `${formatSize(usedBytes)} / ${formatSize(availableBytes)}`;

    return (
        <footer
            aria-label="Filesystem information"
            className="flex flex-wrap items-center justify-end gap-x-5 gap-y-1 border-t border-slate-800 bg-slate-900/35 px-3 py-2 font-mono text-xs text-slate-400"
        >
            <span>
                Disk usage: <span className="text-slate-200">{usage}</span>
            </span>
            <span>
                Filesystem:{" "}
                <span className="text-slate-200">
                    {mountPoint?.mount_type ?? "Unavailable"}
                </span>
            </span>
        </footer>
    );
}

/** Owns keyboard focus for the currently visible local file entries. */
function FileTable(props: {
    agent: Agent;
    directoryPath: string;
    files: LsEntry[];
    sort: FileSort | null;
    onSort: (column: FileSortColumn) => void;
}) {
    const tableRef = React.useRef<HTMLTableElement>(null);

    const getEntries = React.useEffectEvent(() =>
        Array.from(
            tableRef.current?.querySelectorAll<HTMLElement>(
                '[data-keyboard-focus-entry="true"]',
            ) ?? [],
        ),
    );

    useArrayKeyboardFocus(getEntries);

    return (
        <div className="overflow-x-auto">
            <table ref={tableRef} className="w-full min-w-[55rem]">
                <thead>
                    <tr className="border-b border-slate-800 bg-surface-raised">
                        <th className="p-1.5 text-left text-sm font-medium text-slate-400 sm:p-2">
                            Select
                        </th>
                        <SortableFileColumnHeader
                            label="Type"
                            column="type"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <SortableFileColumnHeader
                            label="Name"
                            column="name"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <SortableFileColumnHeader
                            label="Size"
                            column="size"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <SortableFileColumnHeader
                            label="Modified"
                            column="modified"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <SortableFileColumnHeader
                            label="Owner"
                            column="owner"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <SortableFileColumnHeader
                            label="Group"
                            column="group"
                            sort={props.sort}
                            onSort={props.onSort}
                        />
                        <th className="p-1.5 text-right text-sm font-medium text-slate-400 sm:p-2">
                            Actions
                        </th>
                    </tr>
                </thead>
                <tbody>
                    {props.files.map((entry) => (
                        <FileEntry
                            key={entry.name}
                            agent={props.agent}
                            directoryPath={props.directoryPath}
                            entry={entry}
                        />
                    ))}
                </tbody>
            </table>
        </div>
    );
}

/** Provides one accessible control that toggles a file-list sort direction. */
function SortableFileColumnHeader(props: {
    label: string;
    column: FileSortColumn;
    sort: FileSort | null;
    onSort: (column: FileSortColumn) => void;
}) {
    const direction =
        props.sort?.column === props.column ? props.sort.direction : null;

    const nextDirection =
        direction === "ascending" ? "descending" : "ascending";

    const SortIcon =
        direction === "ascending"
            ? ChevronUp
            : direction === "descending"
              ? ChevronDown
              : ArrowDownUp;

    return (
        <th
            aria-sort={direction ?? "none"}
            className="p-0.5 text-left text-sm font-medium text-slate-400"
        >
            <Button
                type="button"
                variant="ghost"
                onClick={() => props.onSort(props.column)}
                aria-label={`Sort by ${props.label} ${nextDirection}`}
                className="flex w-full justify-start gap-1.5 px-1.5 py-1.5 text-left text-slate-400 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500"
            >
                {props.label}
                <SortIcon className="h-3.5 w-3.5" />
            </Button>
        </th>
    );
}

/** Owns target validation and fresh-terminal startup for one archive extraction. */
function UnarchiveDialog(props: {
    agent: Agent;
    entryName: string;
    directoryPath: string;
    onClose: () => void;
}) {
    const [destination, setDestination] =
        React.useState<UnarchiveDestination>("current");

    const [customDirectory, setCustomDirectory] = React.useState("");
    const [errorMessage, setErrorMessage] = React.useState<string | null>(null);
    const terminals = useTerminals();
    const archive = getArchiveInfo(props.entryName);

    /** Validates before opening so invalid shell input remains visible and actionable. */
    const handleSubmit = (event: React.FormEvent<HTMLFormElement>) => {
        event.preventDefault();

        if (destination === "custom") {
            const error = getCustomArchiveDirectoryError(customDirectory);

            if (error) {
                setErrorMessage(error);

                return;
            }
        }

        const command = buildUnarchiveCommand(
            props.entryName,
            destination,
            customDirectory,
        );

        if (!command) {
            setErrorMessage("Unable to create a safe extraction command");

            return;
        }

        props.onClose();
        terminals.create({
            agent: props.agent,
            cwd: props.directoryPath,
            startupCommand: command,
            refreshTarget: {
                agentId: props.agent.id,
                path: props.directoryPath,
            },
        });
    };

    /** Clears stale validation whenever a different target policy is selected. */
    const selectDestination = (nextDestination: UnarchiveDestination) => {
        setDestination(nextDestination);
        setErrorMessage(null);
    };

    return (
        <Dialog
            isOpen={true}
            title="Unarchive"
            description={`Choose where to extract ${props.entryName}. A new terminal will run the extraction command.`}
            closeAriaLabel="Close unarchive dialog"
            errorMessage={errorMessage}
            onClose={props.onClose}
        >
            <form noValidate onSubmit={handleSubmit}>
                <RadioCardGroup
                    legend="Extraction destination"
                    legendClassName="mt-4 text-sm font-medium text-slate-200"
                    optionsClassName="mt-2"
                >
                    <RadioCardOption
                        name="unarchive-destination"
                        value="current"
                        label="Current directory"
                        description={props.directoryPath}
                        checked={destination === "current"}
                        layout="descriptive"
                        onChange={() => selectDestination("current")}
                    />
                    <RadioCardOption
                        name="unarchive-destination"
                        value="subdirectory"
                        label={`Subdirectory ${archive?.directoryName ?? ""}`}
                        description="Create a directory named after the archive and extract into it."
                        checked={destination === "subdirectory"}
                        layout="descriptive"
                        onChange={() => selectDestination("subdirectory")}
                    />
                    <RadioCardOption
                        name="unarchive-destination"
                        value="custom"
                        label="Custom directory"
                        description="Create one directory with a name you choose and extract into it."
                        checked={destination === "custom"}
                        layout="descriptive"
                        onChange={() => selectDestination("custom")}
                    />
                </RadioCardGroup>
                {destination === "custom" ? (
                    <TextField
                        label="Target directory name"
                        value={customDirectory}
                        placeholder="extracted files"
                        description="Enter one directory name beneath the archive's containing directory."
                        required
                        autoFocus
                        className="mt-4"
                        disabled={false}
                        onChange={(value) => {
                            setCustomDirectory(value);
                            setErrorMessage(null);
                        }}
                    />
                ) : null}
                <DialogActions>
                    <Button
                        type="button"
                        variant="secondary"
                        onClick={props.onClose}
                    >
                        Cancel
                    </Button>
                    <Button type="submit">Unarchive</Button>
                </DialogActions>
            </form>
        </Dialog>
    );
}

/** Keeps mutations and download confirmation behind one compact row menu. */
function FileEntryActions(props: {
    agent: Agent;
    entryName: string;
    fullPath: string;
    directoryPath: string;
    isDirectory: boolean;
}) {
    const [isDeleteDialogOpen, setIsDeleteDialogOpen] = React.useState(false);

    const [isDownloadDialogOpen, setIsDownloadDialogOpen] =
        React.useState(false);

    const [isUnarchiveDialogOpen, setIsUnarchiveDialogOpen] =
        React.useState(false);

    const entryType = props.isDirectory ? "directory" : "file";

    const downloadUrl = props.agent.getRawUrl(props.fullPath, {
        download: true,
    });

    const downloadName = props.isDirectory
        ? getDirectoryArchiveName(props.entryName)
        : props.entryName;

    const archive = props.isDirectory ? null : getArchiveInfo(props.entryName);

    return (
        <>
            <PathActionMenu
                label={`Actions for ${entryType} ${props.entryName}`}
                agent={props.agent}
                path={props.fullPath}
                currentName={props.entryName}
                entryType={entryType}
                navigateAfterRename={false}
                showOpenNatively={true}
                showDownload={true}
                downloadUrl={downloadUrl}
                downloadName={downloadName}
                onDownloadDirectory={() => setIsDownloadDialogOpen(true)}
                showUnarchive={archive !== null}
                onUnarchive={() => setIsUnarchiveDialogOpen(true)}
                onDelete={() => setIsDeleteDialogOpen(true)}
            />
            {isUnarchiveDialogOpen ? (
                <UnarchiveDialog
                    agent={props.agent}
                    entryName={props.entryName}
                    directoryPath={props.directoryPath}
                    onClose={() => setIsUnarchiveDialogOpen(false)}
                />
            ) : null}
            <Dialog
                isOpen={isDownloadDialogOpen}
                title="Download directory"
                description={`${props.entryName} will be streamed as a .tar.gz archive while it is downloaded.`}
                closeAriaLabel="Close directory download dialog"
                onClose={() => setIsDownloadDialogOpen(false)}
            >
                <p className="mt-4 text-sm leading-relaxed text-slate-400">
                    The archive is created on the device as data is sent, so the
                    complete directory is not buffered in memory first.
                </p>
                <DialogActions>
                    <Button
                        type="button"
                        variant="secondary"
                        onClick={() => setIsDownloadDialogOpen(false)}
                    >
                        Cancel
                    </Button>
                    <Button
                        as="a"
                        href={downloadUrl}
                        download={downloadName}
                        onClick={() => setIsDownloadDialogOpen(false)}
                        className="font-semibold"
                    >
                        <Download className="h-4 w-4" />
                        Download .tar.gz
                    </Button>
                </DialogActions>
            </Dialog>
            {isDeleteDialogOpen ? (
                <DeletePathDialog
                    agent={props.agent}
                    path={props.fullPath}
                    name={props.entryName}
                    entryType={entryType}
                    onClose={() => setIsDeleteDialogOpen(false)}
                />
            ) : null}
        </>
    );
}

/** Renders one metadata-rich path row and its selection state. */
function FileEntry(props: {
    agent: Agent;
    directoryPath: string;
    entry: LsEntry;
}) {
    const toggleSelectedFile = useSetAtom(toggleSelectedFileAtom);
    const selectedFileKeys = useAtomValue(selectedFileKeysAtom);
    const entry = props.entry;
    const isDirectory = entry.type === "directory";
    const fullPath = joinBrowserPath(props.directoryPath, entry.name);
    const isSelected = selectedFileKeys.has(`${props.agent.id}:${fullPath}`);
    const entryType = isDirectory ? "directory" : "file";

    return (
        <tr
            className="border-b border-slate-800/60 last:border-b-0 hover:bg-tint"
            aria-label={`${isDirectory ? "Directory" : "File"} entry ${entry.name}`}
        >
            <td className="p-1.5 sm:p-2" aria-label="">
                <Checkbox
                    role="checkbox"
                    label={
                        isSelected
                            ? `Unselect ${entryType} ${entry.name}`
                            : `Select ${entryType} ${entry.name}`
                    }
                    checked={isSelected}
                    onCheckedChange={() =>
                        toggleSelectedFile({
                            agentId: props.agent.id,
                            agentName: props.agent.name,
                            path: fullPath,
                            fileName: entry.name,
                            entryType,
                        })
                    }
                />
            </td>
            <td className="p-1.5 sm:p-2">
                {isDirectory ? (
                    <Folder className="h-5 w-5 text-blue-400" />
                ) : (
                    <File className="h-5 w-5 text-slate-500" />
                )}
            </td>
            <td className="p-1.5 sm:p-2">
                <Link
                    {...browserRoute(props.agent.id, fullPath)}
                    data-keyboard-focus-entry="true"
                    className="block min-w-0 truncate font-medium text-blue-400 hover:underline"
                >
                    {entry.name}
                </Link>
            </td>
            <td
                className={
                    isDirectory
                        ? "p-1.5 text-slate-600 sm:p-2"
                        : "p-1.5 text-slate-400 sm:p-2"
                }
                aria-label={`Size for ${entry.name}`}
            >
                {isDirectory ? "-" : formatSize(entry.size)}
            </td>
            <td
                className="p-1.5 text-slate-400 sm:p-2"
                aria-label={`Modified ${entry.name}`}
            >
                <Tooltip
                    content={formatModifiedAge(entry.modified_at, Date.now())}
                >
                    <time
                        className="whitespace-nowrap"
                        dateTime={new Date(
                            entry.modified_at * 1000,
                        ).toISOString()}
                    >
                        {new Date(entry.modified_at * 1000).toLocaleString()}
                    </time>
                </Tooltip>
            </td>
            <td className="p-1.5 text-slate-400 sm:p-2">
                {entry.owner || "-"}
            </td>
            <td className="p-1.5 text-slate-400 sm:p-2">
                {entry.group || "-"}
            </td>
            <td className="p-1.5 text-right sm:p-2">
                <FileEntryActions
                    agent={props.agent}
                    entryName={entry.name}
                    fullPath={fullPath}
                    directoryPath={props.directoryPath}
                    isDirectory={isDirectory}
                />
            </td>
        </tr>
    );
}
