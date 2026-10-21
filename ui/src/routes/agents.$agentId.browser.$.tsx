import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, redirect, useRouter } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { z } from "zod";
import { useSetAtom } from "jotai";
import { agentStartStatesAtom } from "#ui/agent-start-state";
import {
    type ApiClient,
    type Agent,
    type LsDirectoryResponse,
    type LsFileResponse,
} from "#ui/api-client";
import { isLsDirectoryResponse, isLsFileResponse } from "#ui/ls-response";
import { useUserState } from "#ui/user-state";
import { AgentLifecycle } from "#ui/components/agent-lifecycle";
import { RouteError } from "#ui/components/route-error";
import {
    BrowserHeader,
    UnavailablePathState,
} from "#ui/components/browser/navigation";
import { MissingPathCreationForm } from "#ui/components/browser/missing-path";
import { DirectoryFilesActions } from "#ui/components/browser/directory-actions";
import { SelectedFilesCard } from "#ui/components/browser/selected-files-card";
import { FileList } from "#ui/components/browser/file-list";
import { UploadQueue } from "#ui/components/browser/upload-queue";
import {
    DirectoryDetailView,
    FileDetailView,
} from "#ui/components/browser/metadata";
import { SyncView } from "#ui/components/browser/sync";
import { GitDirectoryView, GitFileView } from "#ui/components/browser/git";
import { FileEditView, FileImageView } from "#ui/components/browser/file-views";
import {
    getImmediateParentPath,
    getPathLoadError,
    sortFileEntries,
} from "#ui/components/browser/utils";
import {
    agentAccountsQueryOptions,
    agentDetailsQueryOptions,
    browserListingQueryOptions,
    fetchQueryUncancelled,
    fileContentQueryOptions,
    gitContextQueryOptions,
    gitDiffQueryOptions,
    gitStatusQueryOptions,
    loadAgent,
    metadataQueryOptions,
    queryKeys,
} from "#ui/queries";
import { useAgents, useRequiredAgent } from "#ui/use-agents";
import { useBrowserRefreshTriggers } from "#ui/components/browser/refresh";
import type { MetadataResponse } from "#bindings/MetadataResponse";
import type { MountPoint } from "#bindings/MountPoint";
import type { GitContextResponse } from "#bindings/GitContextResponse";
import { syntaxLanguageFromFileName } from "#ui/utils/editor-language";

type BrowserSearch = {
    view?: "details" | "sync" | "git";
    line?: number;
    preview?: boolean;
};

type BrowserFullSearch = BrowserSearch & {
    q?: string;
    mode?: "content";
    context?: number;
    timeout?: number;
    hidden?: boolean;
    gitignore?: boolean;
    regex?: boolean;
    gitroot?: boolean;
    case?: "smart" | "sensitive" | "insensitive";
};

/** Accepts booleans from router navigation and directly entered query strings. */
const optionalBooleanSchema = z
    .union([
        z.boolean(),
        z.literal("true").transform(() => true),
        z.literal("false").transform(() => false),
    ])
    .optional()
    .catch(undefined);

/** Accepts only 1-based whole line numbers so garbage query values cannot drive the editor. */
const browserLineSchema = z
    .union([
        z.number().int().gte(1),
        z
            .string()
            .regex(/^[1-9][0-9]*$/)
            .transform(Number)
            .pipe(z.number().int().gte(1)),
    ])
    .optional()
    .catch(undefined);

/** Reads line from the raw query so the loader can preserve it without adding line to loaderDeps. */
function lineFromLocationSearch(searchStr: string): number | undefined {
    const params = new URLSearchParams(
        searchStr.startsWith("?") ? searchStr.slice(1) : searchStr,
    );

    const value = params.get("line");

    return value === null ? undefined : browserLineSchema.parse(value);
}

export const Route = createFileRoute("/agents/$agentId/browser/$")({
    validateSearch: (search): BrowserSearch => ({
        view:
            search.view === "details" ||
            search.view === "sync" ||
            search.view === "git"
                ? search.view
                : undefined,
        line: browserLineSchema.parse(search.line),
        preview: optionalBooleanSchema.parse(search.preview),
    }),
    loaderDeps: ({ search }) => ({ view: search.view }),
    loader: async ({ context, deps, params, location }) => {
        const agent = await loadAgent(context, params.agentId);

        if (agent.status !== "connected" || agent.cwd === null) {
            // Retain the full URL until the agent connects and the path can be resolved.
            return {
                kind: "lifecycle" as const,
                agentId: agent.id,
                path: `/${params._splat ?? ""}`,
                lsResult: null,
            };
        }

        const rawSearch = new URLSearchParams(
            location.searchStr.startsWith("?")
                ? location.searchStr.slice(1)
                : location.searchStr,
        );

        if (rawSearch.has("diff")) {
            throw redirect({
                to: "/agents/$agentId/browser/$",
                params,
                search: {
                    view: deps.view,
                    line: lineFromLocationSearch(location.searchStr),
                },
                replace: true,
            });
        }

        const path = `/${params._splat ?? ""}`;

        const gitContextPromise = fetchQueryUncancelled(context.queryClient, {
            ...gitContextQueryOptions(agent, path),
            staleTime: 0,
        });

        // Mount points label the listing's capacity footer.
        const detailsPromise = fetchQueryUncancelled(
            context.queryClient,
            agentDetailsQueryOptions(agent),
        );

        const listingPromise = fetchQueryUncancelled(context.queryClient, {
            ...browserListingQueryOptions(agent, path),
            // Route invalidation is an explicit refresh even if the mounted listing is retained.
            staleTime: 0,
        });

        // Missing paths still resolve the route so breadcrumbs stay available for correction.
        try {
            const [gitContext, lsResult] = await Promise.all([
                gitContextPromise,
                listingPromise,
                detailsPromise,
            ]);

            if (
                deps.view === "git" &&
                gitContext.status === "outside_worktree"
            ) {
                throw redirect({
                    to: "/agents/$agentId/browser/$",
                    params,
                    search: {},
                    replace: true,
                });
            }

            // Canonicalize the cache identity so aliases refresh the listing the route actually displays.
            context.queryClient.setQueryData(
                queryKeys.browserListing(agent.id, lsResult.path),
                lsResult,
            );

            const metadata = isLsFileResponse(lsResult)
                ? await fetchQueryUncancelled(context.queryClient, {
                      ...metadataQueryOptions(agent, lsResult.path),
                      staleTime: 0,
                  })
                : null;

            if (isLsFileResponse(lsResult)) {
                replaceUnsupportedFileView(params, deps.view, metadata);
                replaceDefaultMarkdownPreview({
                    params,
                    view: deps.view,
                    filePath: lsResult.path,
                    metadata,
                    line: lineFromLocationSearch(location.searchStr),
                    preview: optionalBooleanSchema.parse(
                        rawSearch.get("preview"),
                    ),
                    currentSearch: location.search,
                });

                if (
                    wantsFileContentView(deps.view) &&
                    metadata?.editable === true
                ) {
                    await fetchQueryUncancelled(
                        context.queryClient,
                        fileContentQueryOptions(agent, lsResult.path),
                    );
                }
            }

            if (deps.view === "details" && agent.isRoot) {
                await fetchQueryUncancelled(
                    context.queryClient,
                    agentAccountsQueryOptions(agent),
                );
            }

            if (
                deps.view === "git" &&
                gitContext.status === "inside_worktree"
            ) {
                if (isLsDirectoryResponse(lsResult)) {
                    await fetchQueryUncancelled(context.queryClient, {
                        ...gitStatusQueryOptions(agent, lsResult.path),
                        staleTime: 0,
                    });
                } else {
                    await fetchQueryUncancelled(context.queryClient, {
                        ...gitDiffQueryOptions(agent, [lsResult.path], "full"),
                        staleTime: 0,
                    });
                }
            }

            return {
                kind: "connected" as const,
                agentId: agent.id,
                path,
                lsResult,
                pathError: null,
            };
        } catch (error) {
            const pathError = getPathLoadError(error);

            if (!pathError) {
                throw error;
            }

            const [gitContext] = await Promise.all([
                gitContextPromise,
                detailsPromise,
            ]);

            if (
                deps.view === "git" &&
                gitContext.status === "inside_worktree" &&
                gitContext.entry_type === "missing" &&
                gitContext.tracking_state === "deleted"
            ) {
                await fetchQueryUncancelled(context.queryClient, {
                    ...gitDiffQueryOptions(agent, [path], "full"),
                    staleTime: 0,
                });

                return {
                    kind: "connected" as const,
                    agentId: agent.id,
                    path,
                    lsResult: null,
                    pathError,
                };
            }

            if (
                deps.view === "git" &&
                gitContext.status === "outside_worktree"
            ) {
                throw redirect({
                    to: "/agents/$agentId/browser/$",
                    params,
                    search: {},
                    replace: true,
                });
            }

            return {
                kind: "connected" as const,
                agentId: agent.id,
                path,
                lsResult: null,
                pathError,
            };
        }
    },
    component: FileBrowser,
    errorComponent: RouteError,
});

/** Fixes browser navigation chrome in one route-level frame for every path type and view. */
function BrowserRouteShell(props: {
    agent: Agent;
    agentId: string;
    path: string;
    parentPath: string | null;
    entryType: "directory" | "file";
    activeView: "files" | "details" | "view" | "sync" | "git";
    gitAvailable: boolean;
    editable?: boolean;
    /** Bounds the route to the overlay viewport so CodeMirror, not the page, scrolls. */
    fillAvailableHeight?: boolean;
    pathUnavailable?: boolean;
    startEditingPath?: boolean;
    children: ReactNode;
}) {
    return (
        <div
            className={
                props.fillAvailableHeight === true
                    ? "absolute inset-x-0 top-[var(--top-chrome-height,0px)] bottom-[var(--bottom-chrome-height,0px)] flex min-h-0 flex-col overflow-hidden p-2 lg:p-4"
                    : "flex min-h-full flex-col p-2 lg:p-4"
            }
        >
            <BrowserHeader
                agent={props.agent}
                agentId={props.agentId}
                path={props.path}
                parentPath={props.parentPath}
                entryType={props.entryType}
                activeView={props.activeView}
                editable={props.editable}
                gitAvailable={props.gitAvailable}
                pathUnavailable={props.pathUnavailable}
                startEditingPath={props.startEditingPath}
            />
            <div
                className={
                    props.fillAvailableHeight === true
                        ? "-mx-2 -mb-2 flex min-h-0 flex-1 flex-col overflow-hidden lg:-mx-4 lg:-mb-4"
                        : "-mx-2 -mb-2 lg:-mx-4 lg:-mb-4"
                }
            >
                {props.children}
            </div>
        </div>
    );
}

/**
 * Shows connection controls at the bookmarked URL until filesystem commands are available.
 * Availability follows the live inventory, so a disconnect swaps the view immediately and a
 * reconnect resolves the path again instead of waiting for a reload event.
 */
function FileBrowser() {
    const data = Route.useLoaderData();
    const router = useRouter();
    const agent = useRequiredAgent(data.agentId);
    const isAvailable = agent.status === "connected" && agent.cwd !== null;
    const needsResolution = isAvailable && data.kind === "lifecycle";

    useEffect(() => {
        // The loader found the agent offline, so it resolved no path. Run it again now that
        // the agent can serve one, which also applies the path-dependent redirects.
        if (needsResolution) {
            void router.invalidate();
        }
    }, [needsResolution, router]);

    if (!isAvailable) {
        return <AgentLifecycle agent={agent} preserveDestination />;
    }

    if (data.kind === "lifecycle") {
        return (
            <p role="status" className="p-8 text-sm text-slate-400">
                Opening path...
            </p>
        );
    }

    return <ConnectedFileBrowser data={data} agent={agent} />;
}

/** Mounts filesystem subscriptions only after the agent can serve the requested path. */
function ConnectedFileBrowser(props: {
    data: Exclude<
        ReturnType<typeof Route.useLoaderData>,
        { kind: "lifecycle" }
    >;
    agent: Agent;
}) {
    const data = props.data;
    const agent = props.agent;
    const setStartStates = useSetAtom(agentStartStatesAtom);
    useEffect(() => {
        // Completing startup here must not leave a later disconnect showing optimistic progress.
        setStartStates((states) => {
            if (!states[data.agentId]) return states;
            const next = { ...states };
            delete next[data.agentId];

            return next;
        });
    }, [data.agentId, setStartStates]);
    const { api } = Route.useRouteContext();
    const { agentId, path, lsResult, pathError } = data;
    const search = Route.useSearch();
    // Keep the presentation mode while keyed editors reset their per-file buffers.
    const [isEditorFullWindow, setIsEditorFullWindow] = useState(false);
    useBrowserRefreshTriggers(agentId);

    // The loader primed this, and refreshes replace it in place without reloading the route.
    const { data: gitContext } = useSuspenseQuery(
        gitContextQueryOptions(agent, path),
    );

    const parentPath = getImmediateParentPath(path);
    const gitAvailable = gitContext.status === "inside_worktree";

    if (
        pathError?.type === "missing" &&
        search.view === "git" &&
        gitContext.status === "inside_worktree" &&
        gitContext.entry_type === "missing" &&
        gitContext.tracking_state === "deleted"
    ) {
        return (
            <BrowserRouteShell
                agent={agent}
                agentId={agentId}
                path={path}
                parentPath={parentPath}
                entryType="file"
                activeView="git"
                gitAvailable
            >
                <GitFileView agent={agent} path={path} context={gitContext} />
            </BrowserRouteShell>
        );
    }

    if (pathError) {
        return (
            <BrowserRouteShell
                agent={agent}
                agentId={agentId}
                path={path}
                parentPath={parentPath}
                entryType="directory"
                activeView="files"
                gitAvailable={gitAvailable}
                pathUnavailable
                startEditingPath={pathError.type !== "missing"}
            >
                {pathError.type === "missing" ? (
                    <MissingPathCreationForm
                        key={path}
                        agent={agent}
                        path={path}
                    />
                ) : (
                    <UnavailablePathState
                        agent={agent}
                        path={path}
                        parentPath={parentPath}
                        error={pathError}
                    />
                )}
            </BrowserRouteShell>
        );
    }

    if (isLsDirectoryResponse(lsResult)) {
        return (
            <DirectoryBrowserPage
                api={api}
                agent={agent}
                path={path}
                parentPath={parentPath}
                lsResult={lsResult}
                view={search.view}
                gitAvailable={gitAvailable}
            />
        );
    }

    if (isLsFileResponse(lsResult)) {
        return (
            <FileBrowserPage
                api={api}
                agent={agent}
                path={path}
                parentPath={parentPath}
                lsResult={lsResult}
                gitContext={gitContext}
                isEditorFullWindow={isEditorFullWindow}
                onToggleEditorFullWindow={() =>
                    setIsEditorFullWindow((current) => !current)
                }
            />
        );
    }

    return null;
}

/** Picks the view for one file from its live metadata, which shares the route's cache. */
function FileBrowserPage(props: {
    api: ApiClient;
    agent: Agent;
    path: string;
    parentPath: string | null;
    lsResult: LsFileResponse;
    gitContext: GitContextResponse;
    isEditorFullWindow: boolean;
    onToggleEditorFullWindow: () => void;
}) {
    const agent = props.agent;
    const lsResult = props.lsResult;
    const path = props.path;
    const navigate = Route.useNavigate();
    const search = Route.useSearch();
    const agents = useAgents();

    const { data: metadata } = useSuspenseQuery(
        metadataQueryOptions(agent, lsResult.path),
    );

    const fileName = path.split("/").pop() || lsResult.path;
    const downloadUrl = agent.getRawUrl(lsResult.path);
    const editable = metadata.editable;
    const viewableImage = metadata.viewable_image;

    const activeView =
        search.view === "git"
            ? "git"
            : search.view === "sync"
              ? "sync"
              : search.view === "details"
                ? "details"
                : "view";

    const isEditView = activeView === "view" && editable;

    const content =
        activeView === "git" ? (
            <GitFileView
                agent={agent}
                path={lsResult.path}
                context={props.gitContext}
            />
        ) : activeView === "sync" ? (
            <SyncView
                key={`${agent.id}:${lsResult.path}`}
                api={props.api}
                sourceAgent={agent}
                agents={agents}
                sourcePath={lsResult.path}
                entryType="file"
            />
        ) : activeView === "view" && editable ? (
            <FileEditView
                key={`${agent.id}:${lsResult.path}`}
                isFullWindow={props.isEditorFullWindow}
                onToggleFullWindow={props.onToggleEditorFullWindow}
                agent={agent}
                fileName={fileName}
                filePath={lsResult.path}
                repositoryRoot={
                    props.gitContext.status === "inside_worktree"
                        ? props.gitContext.repository_root
                        : null
                }
                mimeType={metadata.mime_type}
                downloadUrl={agent.getRawUrl(lsResult.path, {
                    download: true,
                })}
                scrollToLine={search.line}
                preview={search.preview === true}
                onPreviewChange={(preview) =>
                    navigate({
                        search: (previous) => ({
                            ...previous,
                            preview,
                        }),
                    })
                }
            />
        ) : activeView === "view" && viewableImage ? (
            <FileImageView
                agent={agent}
                path={path}
                fileName={fileName}
                downloadUrl={downloadUrl}
            />
        ) : (
            <FileDetailView
                agent={agent}
                path={path}
                fileName={fileName}
                lsResult={lsResult}
                downloadUrl={downloadUrl}
            />
        );

    return (
        <BrowserRouteShell
            agent={agent}
            agentId={agent.id}
            path={path}
            parentPath={props.parentPath}
            entryType="file"
            activeView={activeView}
            editable={editable}
            gitAvailable={props.gitContext.status === "inside_worktree"}
            fillAvailableHeight={isEditView}
        >
            {content}
        </BrowserRouteShell>
    );
}

/** Owns directory-only filtering and representation selection outside file dispatch. */
function DirectoryBrowserPage(props: {
    api: ApiClient;
    agent: Agent;
    path: string;
    parentPath: string | null;
    lsResult: LsDirectoryResponse;
    view?: "details" | "sync" | "git";
    gitAvailable: boolean;
}) {
    const [userState, setUserState] = useUserState();
    const agents = useAgents();

    const { data: details } = useSuspenseQuery(
        agentDetailsQueryOptions(props.agent),
    );

    const listingQuery = useQuery(
        browserListingQueryOptions(props.agent, props.lsResult.path),
    );

    const lsResult =
        listingQuery.data && isLsDirectoryResponse(listingQuery.data)
            ? listingQuery.data
            : props.lsResult;

    const showHiddenFiles = userState.showHiddenFiles;

    const directories = sortFileEntries(
        lsResult.files.filter((file) => file.type === "directory"),
    );

    const regularFiles = sortFileEntries(
        lsResult.files.filter((file) => file.type === "file"),
    );

    const activeView =
        props.view === "git"
            ? "git"
            : props.view === "details"
              ? "details"
              : props.view === "sync"
                ? "sync"
                : "files";

    return (
        <BrowserRouteShell
            agent={props.agent}
            agentId={props.agent.id}
            path={props.path}
            parentPath={props.parentPath}
            entryType="directory"
            activeView={activeView}
            gitAvailable={props.gitAvailable}
        >
            {activeView === "git" ? (
                <GitDirectoryView agent={props.agent} path={lsResult.path} />
            ) : activeView === "details" ? (
                <DirectoryDetailView
                    agent={props.agent}
                    path={props.path}
                    directoryName={
                        props.path.split("/").filter(Boolean).pop() ?? "/"
                    }
                    lsResult={lsResult}
                />
            ) : activeView === "sync" ? (
                <SyncView
                    key={`${props.agent.id}:${lsResult.path}`}
                    api={props.api}
                    sourceAgent={props.agent}
                    agents={agents}
                    sourcePath={lsResult.path}
                    entryType="directory"
                />
            ) : (
                <>
                    <UploadQueue
                        agentId={props.agent.id}
                        destinationPath={lsResult.path}
                    />
                    <SelectedFilesCard
                        api={props.api}
                        agents={agents}
                        destinationAgent={props.agent}
                        directoryPath={lsResult.path}
                        destinationFileNames={lsResult.files.map(
                            (file) => file.name,
                        )}
                    />
                    <FileList
                        key={lsResult.path}
                        agent={props.agent}
                        agentId={props.agent.id}
                        agentName={props.agent.name}
                        directoryPath={lsResult.path}
                        files={[...directories, ...regularFiles]}
                        showHiddenFiles={showHiddenFiles}
                        mountPoint={getMountPointForPath(
                            details.mount_points,
                            lsResult.path,
                        )}
                        actions={
                            <DirectoryFilesActions
                                agent={props.agent}
                                directoryPath={lsResult.path}
                                showHiddenFiles={showHiddenFiles}
                                onToggleHiddenFiles={() =>
                                    setUserState((current) => ({
                                        ...current,
                                        showHiddenFiles:
                                            !current.showHiddenFiles,
                                    }))
                                }
                            />
                        }
                    />
                </>
            )}
        </BrowserRouteShell>
    );
}

/** Content is the file default view. */
function wantsFileContentView(view: BrowserSearch["view"]) {
    return view === undefined;
}

/** Replaces history so a content URL for a file without viewable content does not stay on the stack. */
function replaceUnsupportedFileView(
    params: { agentId: string; _splat?: string },
    view: BrowserSearch["view"],
    metadata: MetadataResponse | null,
) {
    const canShowContent =
        metadata?.editable === true || metadata?.viewable_image === true;

    if (wantsFileContentView(view) && !canShowContent) {
        throw redirect({
            to: "/agents/$agentId/browser/$",
            params,
            search: { view: "details" },
            replace: true,
        });
    }
}

/** Makes rendered markdown the canonical first view while line bookmarks stay in the editor. */
function replaceDefaultMarkdownPreview(options: {
    params: { agentId: string; _splat?: string };
    view: BrowserSearch["view"];
    filePath: string;
    metadata: MetadataResponse | null;
    line: BrowserSearch["line"];
    preview: BrowserSearch["preview"];
    currentSearch: BrowserFullSearch;
}) {
    if (
        !wantsFileContentView(options.view) ||
        options.metadata?.editable !== true ||
        syntaxLanguageFromFileName(options.filePath) !== "markdown" ||
        options.line !== undefined ||
        options.preview !== undefined
    ) {
        return;
    }

    throw redirect({
        to: "/agents/$agentId/browser/$",
        params: options.params,
        search: { ...options.currentSearch, preview: true },
        replace: true,
    });
}

/** Selects the most specific filesystem mount containing the browsed directory. */
function getMountPointForPath(
    mountPoints: MountPoint[],
    path: string,
): MountPoint | null {
    return (
        mountPoints
            .filter(
                (mountPoint) =>
                    mountPoint.path === "/" ||
                    path === mountPoint.path ||
                    path.startsWith(`${mountPoint.path}/`),
            )
            .sort((left, right) => right.path.length - left.path.length)[0] ??
        null
    );
}
