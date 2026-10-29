import type { Agent } from "#ui/api-client";
import { Button } from "#ui/components/button";
import { BrowserViewCard } from "#ui/components/browser-view-card";
import { CopyableGitRef } from "#ui/components/copyable-git-ref";
import {
    MetadataItem,
    MetadataSectionsLayout,
    PathDetailHeader,
    PermissionsSection,
    ShareableLinksSection,
    SizeAndEntryItems,
} from "#ui/components/browser/metadata";
import { PersistentPathActions } from "#ui/components/browser/path-actions";
import { getBaseName } from "#ui/utils/path";
import { getDirectoryArchiveName } from "#ui/utils/archive";
import type { GitRevisionEntry } from "#bindings/GitRevisionEntry";
import type { GitRevisionTreeResponse } from "#bindings/GitRevisionTreeResponse";

/** Converts a Git entry's recorded mode into Unix permission bits, when Git records any. */
function revisionPermissions(entry: GitRevisionEntry): number | null {
    switch (entry.type) {
        case "file":
            return entry.executable ? 0o755 : 0o644;
        case "symlink":
            return 0o777;
        case "directory":
        case "submodule":
            return null;
    }
}

/** Names the Git mode a revision entry was committed with, as `git ls-tree` prints it. */
function revisionMode(entry: GitRevisionEntry): string {
    switch (entry.type) {
        case "file":
            return entry.executable ? "100755" : "100644";
        case "symlink":
            return "120000";
        case "directory":
            return "040000";
        case "submodule":
            return "160000";
    }
}

/**
 * Presents a path at a Git revision with the same header, metadata grid, permissions and
 * shareable links as the worktree details, filled with what Git records. Ownership and
 * permissions are not editable and every action that would touch the working file is
 * disabled; Download and Select read the commit instead.
 */
export function RevisionDetailView(props: {
    agent: Agent;
    tree: GitRevisionTreeResponse;
}) {
    const entry = props.tree.entry;
    const isDirectory = entry.type === "directory";
    const entryType = isDirectory ? "directory" : "file";
    const name = getBaseName(props.tree.path) || props.tree.path;
    const headingPrefix = isDirectory ? "directory" : "file";
    const permissions = revisionPermissions(entry);
    // Git stores content and mode only, never ownership.
    const notRecorded = "Not in Git";

    return (
        <BrowserViewCard>
            <PathDetailHeader
                entryType={entryType}
                name={name}
                path={props.tree.path}
                canCopyPath={!isDirectory}
                nameActions={
                    <PersistentPathActions
                        agent={props.agent}
                        path={props.tree.path}
                        currentName={name}
                        entryType={entryType}
                        view="details"
                        downloadUrl={props.agent.getGitDownloadUrl(
                            props.tree.path,
                            props.tree.commit.id,
                        )}
                        downloadName={
                            isDirectory ? getDirectoryArchiveName(name) : name
                        }
                        revision={{
                            commitId: props.tree.commit.id,
                            // Git records no content a copy or download could write for these.
                            transferable:
                                entry.type === "file" ||
                                entry.type === "directory",
                        }}
                    />
                }
            />
            <MetadataSectionsLayout
                headingPrefix={headingPrefix}
                description="Identity and content as recorded in Git at this revision."
                items={
                    <>
                        <SizeAndEntryItems
                            size={
                                entry.type === "file" ? entry.size : undefined
                            }
                            sizeLabel={
                                isDirectory ? "Directory size value" : undefined
                            }
                            sizeAction={
                                isDirectory ? (
                                    <Button
                                        tooltip="Directory sizes are only calculated for working files."
                                        type="button"
                                        variant="subtle"
                                        disabled
                                    >
                                        Calculate size
                                    </Button>
                                ) : undefined
                            }
                            entryCount={
                                entry.type === "directory"
                                    ? entry.entries.length
                                    : undefined
                            }
                        />
                        <MetadataItem label="Owner" value={notRecorded} />
                        <MetadataItem label="Group" value={notRecorded} />
                        <MetadataItem
                            label="Mode"
                            value={revisionMode(entry)}
                            mono
                        />
                        {entry.type === "file" ? (
                            <MetadataItem
                                label="Blob"
                                value={
                                    <CopyableGitRef
                                        kind="blob"
                                        value={entry.blob_id}
                                    >
                                        <span className="truncate">
                                            {entry.blob_id}
                                        </span>
                                    </CopyableGitRef>
                                }
                                valueLabel="Blob id"
                                mono
                            />
                        ) : null}
                        {entry.type === "symlink" ? (
                            <MetadataItem
                                label="Target"
                                value={entry.target}
                                mono
                            />
                        ) : null}
                        {entry.type === "submodule" ? (
                            <MetadataItem
                                label="Submodule commit"
                                value={
                                    <CopyableGitRef
                                        kind="commit"
                                        value={entry.commit_id}
                                    >
                                        <span className="truncate">
                                            {entry.commit_id}
                                        </span>
                                    </CopyableGitRef>
                                }
                                mono
                            />
                        ) : null}
                        <MetadataItem
                            label="Commit"
                            value={
                                <CopyableGitRef
                                    kind="commit"
                                    value={props.tree.commit.id}
                                >
                                    <span className="truncate">
                                        {props.tree.commit.id}
                                    </span>
                                </CopyableGitRef>
                            }
                            valueLabel="Commit id"
                            mono
                        />
                    </>
                }
                permissions={
                    permissions === null ? (
                        <p className="text-sm text-slate-500">
                            Git does not record permissions for directories.
                        </p>
                    ) : (
                        <PermissionsSection
                            headingId={`${headingPrefix}-permissions-heading`}
                            permissions={permissions}
                            interactive={false}
                            onToggleBit={() => {}}
                        />
                    )
                }
            />
            {isDirectory ? null : (
                <ShareableLinksSection
                    downloadUrl=""
                    oneTimeTokens={[]}
                    isCreating={false}
                    errorMessage={null}
                    onCreate={() => {}}
                    disabled
                />
            )}
        </BrowserViewCard>
    );
}
