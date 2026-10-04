import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, RotateCw, ShieldX } from "lucide-react";

import type { McpTokenMetadataResponse, McpTokenScope } from "#ui/api-client";
import { Route as RootRoute } from "./__root";
import { mcpTokensQueryOptions, queryKeys } from "#ui/queries";
import { Button } from "#ui/components/button";
import { Checkbox } from "#ui/components/checkbox";
import { ConfirmationDialog } from "#ui/components/confirmation-dialog";
import { CopyableCodeRow } from "#ui/components/copyable-code-row";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { Select } from "#ui/components/select";
import { TextField } from "#ui/components/text-field";
import { Toast } from "#ui/components/toast";
import { Tooltip } from "#ui/components/tooltip";

export const Route = createFileRoute("/settings/mcp")({
    loader: async ({ context }) => {
        await context.queryClient.ensureQueryData(
            mcpTokensQueryOptions(context.api),
        );
    },
    component: McpTokenSettingsPage,
});

const scopeOptions: Array<{
    value: McpTokenScope;
    label: string;
    description: string;
}> = [
    {
        value: "devices:read",
        label: "Device inventory",
        description: "List allowed devices and read their status.",
    },
    {
        value: "files:read",
        label: "Read files",
        description: "List, search, read, and download files.",
    },
    {
        value: "files:write",
        label: "Write files",
        description: "Upload, replace, copy, and create files or directories.",
    },
    {
        value: "files:delete",
        label: "Delete files",
        description: "Trash or permanently remove files and move sources.",
    },
    {
        value: "agents:manage",
        label: "Manage agents",
        description: "Start and stop allowed managed devices.",
    },
    {
        value: "commands:execute",
        label: "Run shell commands",
        description:
            "High trust: commands can read, modify, or delete files regardless of file-tool scopes.",
    },
];

/** Owns transient secrets only while their modal is open and keeps them outside Query caches. */
function McpTokenSettingsPage() {
    const { api } = RootRoute.useRouteContext();
    const { agents } = RootRoute.useLoaderData();
    const queryClient = useQueryClient();
    const tokensQuery = useQuery(mcpTokensQueryOptions(api));
    const [createOpen, setCreateOpen] = React.useState(false);
    const [secret, setSecret] = React.useState<{
        value: string;
        action: "created" | "rotated";
    } | null>(null);
    const [selected, setSelected] = React.useState<{
        token: McpTokenMetadataResponse;
        action: "rotate" | "revoke";
    } | null>(null);
    const [notice, setNotice] = React.useState<string | null>(null);
    const refresh = async () =>
        queryClient.invalidateQueries({ queryKey: queryKeys.mcpTokens() });
    const rotate = useMutation({
        mutationFn: (id: string) => api.rotateMcpToken(id),
        onSuccess: async (result) => {
            setSelected(null);
            setSecret({ value: result.token, action: "rotated" });
            await refresh();
        },
    });
    const revoke = useMutation({
        mutationFn: (id: string) => api.revokeMcpToken(id),
        onSuccess: async () => {
            setSelected(null);
            setNotice(
                "Token revoked. Future MCP and transfer requests will be rejected.",
            );
            await refresh();
        },
    });
    const tokens = tokensQuery.data?.tokens ?? [];

    return (
        <div className="p-4 sm:p-8">
            <div className="mx-auto max-w-5xl">
                <div className="flex flex-wrap items-start justify-between gap-4">
                    <div>
                        <div className="flex items-center gap-3">
                            <KeyRound className="h-6 w-6 text-blue-400" />
                            <h1 className="text-2xl font-bold text-slate-100">
                                MCP tokens
                            </h1>
                        </div>
                        <p className="mt-2 max-w-2xl text-sm text-slate-400">
                            Create bearer tokens for configured MCP clients.
                            Automatic OAuth sign-in is not supported.
                        </p>
                    </div>
                    <Tooltip content="Create a scoped MCP bearer token">
                        <Button
                            type="button"
                            onClick={() => setCreateOpen(true)}
                        >
                            <Plus className="h-4 w-4" />
                            Create token
                        </Button>
                    </Tooltip>
                </div>
                {tokensQuery.isError ? (
                    <p
                        role="alert"
                        className="mt-6 rounded border border-red-800 bg-red-950/20 p-4 text-red-300"
                    >
                        Could not load MCP tokens.{" "}
                        <Button
                            type="button"
                            variant="subtle"
                            onClick={() => void tokensQuery.refetch()}
                        >
                            Try again
                        </Button>
                    </p>
                ) : tokensQuery.isLoading ? (
                    <p className="mt-8 text-slate-400">Loading MCP tokens…</p>
                ) : tokens.length === 0 ? (
                    <div className="mt-8 rounded-lg border border-dashed border-slate-700 p-8 text-center text-slate-400">
                        No MCP tokens yet. Create one to connect a client.
                    </div>
                ) : (
                    <div className="mt-6 grid gap-4">
                        {tokens.map((token) => (
                            <TokenCard
                                key={token.credential_id}
                                token={token}
                                agents={agents}
                                onRotate={() =>
                                    setSelected({ token, action: "rotate" })
                                }
                                onRevoke={() =>
                                    setSelected({ token, action: "revoke" })
                                }
                            />
                        ))}
                    </div>
                )}
            </div>
            <CreateTokenDialog
                isOpen={createOpen}
                agents={agents}
                isBusy={false}
                onClose={() => setCreateOpen(false)}
                onCreated={async (value) => {
                    setCreateOpen(false);
                    setSecret({ value, action: "created" });
                    await refresh();
                }}
            />
            <ConfirmationDialog
                isOpen={selected?.action === "rotate"}
                title="Rotate MCP token?"
                description="Rotation immediately invalidates the old bearer token while preserving this grant and operation ownership."
                confirmLabel="Rotate token"
                busyLabel="Rotating…"
                isBusy={rotate.isPending}
                errorMessage={
                    rotate.error instanceof Error ? rotate.error.message : null
                }
                onClose={() => setSelected(null)}
                onConfirm={() => {
                    if (selected) rotate.mutate(selected.token.credential_id);
                }}
            />
            <ConfirmationDialog
                isOpen={selected?.action === "revoke"}
                title="Revoke MCP token?"
                description="All later MCP and streaming requests using this credential will fail immediately."
                confirmLabel="Revoke token"
                busyLabel="Revoking…"
                isBusy={revoke.isPending}
                errorMessage={
                    revoke.error instanceof Error ? revoke.error.message : null
                }
                onClose={() => setSelected(null)}
                onConfirm={() => {
                    if (selected) revoke.mutate(selected.token.credential_id);
                }}
            />
            <SecretDialog
                secret={secret}
                endpoint={`${window.location.origin}/mcp`}
                onClose={() => setSecret(null)}
            />
            {notice ? (
                <Toast tone="success" onDismiss={() => setNotice(null)}>
                    {notice}
                </Toast>
            ) : null}
        </div>
    );
}

/** Presents one secret-free record and disables lifecycle actions for inactive credentials. */
function TokenCard(props: {
    token: McpTokenMetadataResponse;
    agents: Array<{ id: string; name: string }>;
    onRotate: () => void;
    onRevoke: () => void;
}) {
    const now = Date.now();
    const expired =
        props.token.expires_at !== null &&
        Date.parse(props.token.expires_at) <= now;
    const status =
        props.token.revoked_at !== null
            ? "Revoked"
            : expired
              ? "Expired"
              : "Active";
    const deviceNames = props.token.device_ids.map(
        (id) => props.agents.find((agent) => agent.id === id)?.name ?? id,
    );
    return (
        <article className="rounded-lg border border-slate-800 bg-slate-900/40 p-5">
            <div className="flex flex-wrap justify-between gap-4">
                <div>
                    <div className="flex items-center gap-2">
                        <h2 className="font-semibold text-slate-100">
                            {props.token.name}
                        </h2>
                        <span
                            className={
                                status === "Active"
                                    ? "rounded bg-emerald-500/15 px-2 py-0.5 text-xs text-emerald-300"
                                    : "rounded bg-slate-700 px-2 py-0.5 text-xs text-slate-300"
                            }
                        >
                            {status}
                        </span>
                    </div>
                    <dl className="mt-3 grid gap-1 text-sm text-slate-400">
                        <div>
                            <dt className="inline text-slate-500">Scopes: </dt>
                            <dd className="inline">
                                {props.token.scopes.length
                                    ? props.token.scopes.join(", ")
                                    : "Hello only"}
                            </dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">Devices: </dt>
                            <dd className="inline">
                                {deviceNames.length
                                    ? deviceNames.join(", ")
                                    : "None"}
                            </dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">Expires: </dt>
                            <dd className="inline">
                                {props.token.expires_at
                                    ? new Date(
                                          props.token.expires_at,
                                      ).toLocaleString()
                                    : "Never"}
                            </dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">
                                Last used:{" "}
                            </dt>
                            <dd className="inline">
                                {props.token.last_used_at
                                    ? new Date(
                                          props.token.last_used_at,
                                      ).toLocaleString()
                                    : "Never"}
                            </dd>
                        </div>
                    </dl>
                </div>
                <div className="flex items-start gap-2">
                    <Button
                        type="button"
                        variant="secondary"
                        disabled={status !== "Active"}
                        onClick={props.onRotate}
                    >
                        <RotateCw className="h-4 w-4" />
                        Rotate
                    </Button>
                    <Button
                        type="button"
                        variant="danger"
                        disabled={status !== "Active"}
                        onClick={props.onRevoke}
                    >
                        <ShieldX className="h-4 w-4" />
                        Revoke
                    </Button>
                </div>
            </div>
        </article>
    );
}

/** Collects an explicit grant; read-only scopes are the safe default and devices default empty. */
function CreateTokenDialog(props: {
    isOpen: boolean;
    agents: Array<{ id: string; name: string }>;
    isBusy: boolean;
    onClose: () => void;
    onCreated: (secret: string) => Promise<void>;
}) {
    const { api } = RootRoute.useRouteContext();
    const [name, setName] = React.useState("");
    const [expiry, setExpiry] = React.useState("30");
    const [scopes, setScopes] = React.useState<McpTokenScope[]>([
        "devices:read",
        "files:read",
    ]);
    const [devices, setDevices] = React.useState<string[]>([]);
    const mutation = useMutation({
        mutationFn: () =>
            api.createMcpToken({
                name,
                scopes,
                device_ids: devices,
                expires_at:
                    expiry === "never"
                        ? null
                        : new Date(
                              Date.now() + Number(expiry) * 86_400_000,
                          ).toISOString(),
            }),
        onSuccess: (result) => props.onCreated(result.token),
    });
    const close = () => {
        if (!mutation.isPending) {
            setName("");
            setExpiry("30");
            setScopes(["devices:read", "files:read"]);
            setDevices([]);
            mutation.reset();
            props.onClose();
        }
    };
    return (
        <Dialog
            isOpen={props.isOpen}
            title="Create MCP token"
            description="Choose the least access this client needs. Empty device access intentionally grants no device tools."
            closeAriaLabel="Close token creation"
            isBusy={mutation.isPending}
            errorMessage={
                mutation.error instanceof Error ? mutation.error.message : null
            }
            size="wide"
            onClose={close}
        >
            <div className="mt-5 grid gap-6 md:grid-cols-2">
                <TextField
                    label="Token name"
                    value={name}
                    placeholder="Coding assistant"
                    description="Shown only in Redoor token management."
                    required
                    disabled={mutation.isPending}
                    onChange={setName}
                />
                <label className="text-sm font-medium text-slate-300">
                    Expiry
                    <Select
                        aria-label="Token expiry"
                        value={expiry}
                        disabled={mutation.isPending}
                        containerClassName="mt-2 block"
                        onChange={(event) => setExpiry(event.target.value)}
                    >
                        <option value="7">7 days</option>
                        <option value="30">30 days</option>
                        <option value="90">90 days</option>
                        <option value="never">Never</option>
                    </Select>
                    <span className="mt-1.5 block text-xs font-normal text-slate-500">
                        Short expiries limit exposure if a client leaks its
                        token.
                    </span>
                </label>
                <fieldset className="space-y-2">
                    <legend className="mb-2 text-sm font-medium text-slate-300">
                        Permissions
                    </legend>
                    {scopeOptions.map((option) => (
                        <Checkbox
                            key={option.value}
                            role="checkbox"
                            checked={scopes.includes(option.value)}
                            disabled={mutation.isPending}
                            onCheckedChange={(checked) =>
                                setScopes((current) =>
                                    checked
                                        ? [...current, option.value]
                                        : current.filter(
                                              (scope) => scope !== option.value,
                                          ),
                                )
                            }
                        >
                            <span>
                                <span className="block text-left">
                                    {option.label}
                                </span>
                                <span
                                    className={
                                        option.value === "commands:execute"
                                            ? "block max-w-sm text-left text-xs text-amber-300"
                                            : "block max-w-sm text-left text-xs text-slate-500"
                                    }
                                >
                                    {option.description}
                                </span>
                            </span>
                        </Checkbox>
                    ))}
                </fieldset>
                <fieldset className="space-y-2">
                    <legend className="mb-2 text-sm font-medium text-slate-300">
                        Allowed devices
                    </legend>
                    {props.agents.length === 0 ? (
                        <p className="text-sm text-slate-500">
                            No configured devices are available.
                        </p>
                    ) : (
                        props.agents.map((agent) => (
                            <Checkbox
                                key={agent.id}
                                role="checkbox"
                                checked={devices.includes(agent.id)}
                                disabled={mutation.isPending}
                                onCheckedChange={(checked) =>
                                    setDevices((current) =>
                                        checked
                                            ? [...current, agent.id]
                                            : current.filter(
                                                  (id) => id !== agent.id,
                                              ),
                                    )
                                }
                            >
                                {agent.name}
                            </Checkbox>
                        ))
                    )}
                    <p className="text-xs text-slate-500">
                        Select each device explicitly. No selection grants no
                        device access.
                    </p>
                </fieldset>
            </div>
            <DialogActions stackOnMobile>
                <Button
                    type="button"
                    variant="secondary"
                    disabled={mutation.isPending}
                    onClick={close}
                >
                    Cancel
                </Button>
                <Button
                    type="button"
                    isLoading={mutation.isPending}
                    disabled={name.trim().length === 0}
                    onClick={() => mutation.mutate()}
                >
                    Create token
                </Button>
            </DialogActions>
        </Dialog>
    );
}

/** Displays the sole recoverable copy of a new secret until the user closes the workflow. */
function SecretDialog(props: {
    secret: { value: string; action: "created" | "rotated" } | null;
    endpoint: string;
    onClose: () => void;
}) {
    if (!props.secret) return null;
    return (
        <Dialog
            isOpen
            title={
                props.secret.action === "created"
                    ? "Token created"
                    : "Token rotated"
            }
            description="Copy this bearer token now. Redoor stores only a verifier and cannot show it again."
            closeAriaLabel="Close one-time token"
            onClose={props.onClose}
        >
            <div className="mt-5 space-y-4">
                <CopyableCodeRow
                    label="Bearer token"
                    value={props.secret.value}
                    copyAriaLabel="Copy MCP bearer token"
                />
                <CopyableCodeRow
                    label="MCP endpoint"
                    value={props.endpoint}
                    copyAriaLabel="Copy MCP endpoint"
                />
                <p className="text-sm text-slate-400">
                    Configure your client to POST to this endpoint with{" "}
                    <code className="text-slate-200">
                        Authorization: Bearer &lt;token&gt;
                    </code>{" "}
                    and protocol version{" "}
                    <code className="text-slate-200">2026-07-28</code>.
                </p>
            </div>
            <DialogActions>
                <Button type="button" onClick={props.onClose}>
                    I saved the token
                </Button>
            </DialogActions>
        </Dialog>
    );
}
