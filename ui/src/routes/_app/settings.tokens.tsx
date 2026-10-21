import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Pencil, Plus, RotateCw, Trash2 } from "lucide-react";

import type { AccessTokenResponse } from "#bindings/AccessTokenResponse";
import type { DeviceScope } from "#bindings/DeviceScope";
import type { Permission } from "#bindings/Permission";
import { accessTokensQueryOptions, queryKeys } from "#ui/queries";
import { useAgents } from "#ui/use-agents";
import { Button } from "#ui/components/button";
import { Checkbox } from "#ui/components/checkbox";
import { ConfirmationDialog } from "#ui/components/confirmation-dialog";
import { CopyableCodeRow } from "#ui/components/copyable-code-row";
import { Dialog } from "#ui/components/dialog";
import { DialogActions } from "#ui/components/dialog-actions";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { Select } from "#ui/components/select";
import { TextField } from "#ui/components/text-field";
import { Toast } from "#ui/components/toast";
import { getErrorMessage } from "#ui/utils/error";

export const Route = createFileRoute("/_app/settings/tokens")({
    loader: async ({ context }) => {
        await context.queryClient.ensureQueryData(
            accessTokensQueryOptions(context.api),
        );
    },
    component: AccessTokenSettingsPage,
});

/**
 * Typed as an exhaustive record so a new server-side permission fails the TS
 * build until it gets a human label instead of silently showing a raw id.
 */
const permissionLabels: Record<
    Permission,
    { label: string; description: string }
> = {
    "files:read": {
        label: "Read files",
        description: "List, search, read, and download files.",
    },
    "files:write": {
        label: "Write files",
        description: "Upload, replace, copy, and create files or directories.",
    },
    "files:delete": {
        label: "Delete files",
        description: "Trash or permanently remove files and move sources.",
    },
    "commands:execute": {
        label: "Run commands",
        description:
            "High trust: commands run as the agent's OS user and can bypass all file permissions.",
    },
    "agents:manage": {
        label: "Manage agents",
        description: "Start and stop managed devices.",
    },
};

/** Fixed display order so the least dangerous permissions are listed first. */
const permissionOrder: Permission[] = [
    "files:read",
    "files:write",
    "files:delete",
    "commands:execute",
    "agents:manage",
];

type Agents = Array<{ id: string; name: string }>;

/** Tokens past their expiry are rejected by the server, so the UI must not offer to extend their use. */
function isExpired(token: AccessTokenResponse): boolean {
    return (
        token.expires_at !== null && Date.parse(token.expires_at) <= Date.now()
    );
}

/** Formats optional timestamps the same way everywhere so "Never" is unambiguous. */
function formatTimestamp(value: string | null): string {
    return value ? new Date(value).toLocaleString() : "Never";
}

/** Owns transient secrets only while their modal is open and keeps them outside Query caches. */
function AccessTokenSettingsPage() {
    const { api } = Route.useRouteContext();
    const agents = useAgents();
    const queryClient = useQueryClient();
    const tokensQuery = useQuery(accessTokensQueryOptions(api));
    const [createOpen, setCreateOpen] = React.useState(false);

    const [editing, setEditing] = React.useState<AccessTokenResponse | null>(
        null,
    );

    const [secret, setSecret] = React.useState<{
        value: string;
        action: "created" | "rotated";
    } | null>(null);

    const [selected, setSelected] = React.useState<{
        token: AccessTokenResponse;
        action: "rotate" | "delete";
    } | null>(null);

    const [notice, setNotice] = React.useState<string | null>(null);

    const refresh = async () =>
        queryClient.invalidateQueries({ queryKey: queryKeys.accessTokens() });

    const rotate = useMutation({
        mutationFn: (id: string) => api.rotateAccessToken(id),
        onSuccess: async (result) => {
            setSelected(null);
            setSecret({ value: result.token, action: "rotated" });
            await refresh();
        },
    });

    const remove = useMutation({
        mutationFn: (id: string) => api.deleteAccessToken(id),
        onSuccess: async () => {
            setSelected(null);
            setNotice(
                "Token deleted. Clients using it will be rejected from now on.",
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
                                Access tokens
                            </h1>
                        </div>
                        <p className="mt-2 max-w-2xl text-sm text-slate-400">
                            Access tokens authenticate the remote CLI and MCP
                            clients. Running{" "}
                            <code className="text-slate-200">
                                redoor remote login
                            </code>{" "}
                            creates one automatically; create tokens here for
                            MCP clients.
                        </p>
                    </div>
                    <Button
                        tooltip="Create a token with limited permissions and devices"
                        type="button"
                        onClick={() => setCreateOpen(true)}
                    >
                        <Plus className="h-4 w-4" />
                        Create token
                    </Button>
                </div>
                {tokensQuery.isError ? (
                    <p
                        role="alert"
                        className="mt-6 rounded border border-red-800 bg-red-950/20 p-4 text-red-300"
                    >
                        Could not load access tokens.{" "}
                        <Button
                            tooltip="Reload the token list"
                            type="button"
                            variant="subtle"
                            onClick={() => void tokensQuery.refetch()}
                        >
                            Try again
                        </Button>
                    </p>
                ) : tokensQuery.isLoading ? (
                    <p className="mt-8 text-slate-400">
                        Loading access tokens…
                    </p>
                ) : tokens.length === 0 ? (
                    <div className="mt-8 rounded-lg border border-dashed border-slate-700 p-8 text-center text-slate-400">
                        No access tokens yet. Create one to connect an MCP
                        client.
                    </div>
                ) : (
                    <div className="mt-6 grid gap-4">
                        {tokens.map((token) => (
                            <TokenCard
                                key={token.id}
                                token={token}
                                agents={agents}
                                onEdit={() => setEditing(token)}
                                onRotate={() =>
                                    setSelected({ token, action: "rotate" })
                                }
                                onDelete={() =>
                                    setSelected({ token, action: "delete" })
                                }
                            />
                        ))}
                    </div>
                )}
            </div>
            {createOpen ? (
                <TokenDialog
                    agents={agents}
                    onClose={() => setCreateOpen(false)}
                    onCreated={async (value) => {
                        setCreateOpen(false);
                        setSecret({ value, action: "created" });
                        await refresh();
                    }}
                />
            ) : null}
            {editing ? (
                <TokenDialog
                    key={editing.id}
                    token={editing}
                    agents={agents}
                    onClose={() => setEditing(null)}
                    onUpdated={async () => {
                        setEditing(null);
                        setNotice(
                            "Token updated. New permissions apply to future requests.",
                        );
                        await refresh();
                    }}
                />
            ) : null}
            <ConfirmationDialog
                isOpen={selected?.action === "rotate"}
                title="Rotate access token?"
                description="Rotation immediately invalidates the current secret. Clients must be reconfigured with the new one."
                confirmLabel="Rotate token"
                busyLabel="Rotating…"
                isBusy={rotate.isPending}
                errorMessage={
                    rotate.isError
                        ? getErrorMessage(
                              rotate.error,
                              "Could not rotate the token",
                          )
                        : null
                }
                onClose={() => setSelected(null)}
                onConfirm={() => {
                    if (selected) rotate.mutate(selected.token.id);
                }}
            />
            <ConfirmationDialog
                isOpen={selected?.action === "delete"}
                title="Delete access token?"
                description="All later requests using this token will fail immediately. This cannot be undone."
                confirmLabel="Delete token"
                busyLabel="Deleting…"
                isBusy={remove.isPending}
                errorMessage={
                    remove.isError
                        ? getErrorMessage(
                              remove.error,
                              "Could not delete the token",
                          )
                        : null
                }
                onClose={() => setSelected(null)}
                onConfirm={() => {
                    if (selected) remove.mutate(selected.token.id);
                }}
            />
            <SecretDialog
                secret={secret}
                origin={window.location.origin}
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

/** Presents one secret-free record; expired tokens can only be deleted because the server rejects them. */
function TokenCard(props: {
    token: AccessTokenResponse;
    agents: Agents;
    onEdit: () => void;
    onRotate: () => void;
    onDelete: () => void;
}) {
    const expired = isExpired(props.token);

    const devices =
        props.token.devices.kind === "all"
            ? "All devices"
            : props.token.devices.device_ids
                  .map(
                      (id) =>
                          props.agents.find((agent) => agent.id === id)?.name ??
                          id,
                  )
                  .join(", ");

    const permissions = permissionOrder.filter((permission) =>
        props.token.permissions.includes(permission),
    );

    return (
        <article
            aria-label={props.token.name}
            className="rounded-lg border border-slate-800 bg-slate-900/40 p-5"
        >
            <div className="flex flex-wrap justify-between gap-4">
                <div className="min-w-0">
                    <div className="flex items-center gap-2">
                        <h2 className="font-semibold text-slate-100">
                            {props.token.name}
                        </h2>
                        <span
                            className={
                                expired
                                    ? "rounded bg-slate-700 px-2 py-0.5 text-xs text-slate-300"
                                    : "rounded bg-emerald-500/15 px-2 py-0.5 text-xs text-emerald-300"
                            }
                        >
                            {expired ? "Expired" : "Active"}
                        </span>
                    </div>
                    <ul
                        aria-label="Permissions"
                        className="mt-3 flex flex-wrap gap-1.5"
                    >
                        {permissions.map((permission) => (
                            <li
                                key={permission}
                                className={
                                    permission === "commands:execute"
                                        ? "rounded bg-amber-500/15 px-2 py-0.5 text-xs text-amber-300"
                                        : "rounded bg-blue-500/15 px-2 py-0.5 text-xs text-blue-300"
                                }
                            >
                                {permissionLabels[permission].label}
                            </li>
                        ))}
                    </ul>
                    <dl className="mt-3 grid gap-1 text-sm text-slate-400">
                        <div>
                            <dt className="inline text-slate-500">Devices: </dt>
                            <dd className="inline">{devices}</dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">Created: </dt>
                            <dd className="inline">
                                {formatTimestamp(props.token.created_at)}
                            </dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">Expires: </dt>
                            <dd className="inline">
                                {formatTimestamp(props.token.expires_at)}
                            </dd>
                        </div>
                        <div>
                            <dt className="inline text-slate-500">
                                Last used:{" "}
                            </dt>
                            <dd className="inline">
                                {formatTimestamp(props.token.last_used_at)}
                            </dd>
                        </div>
                    </dl>
                </div>
                <div className="flex items-start gap-2">
                    <Button
                        tooltip="Edit token name, permissions, and allowed devices"
                        type="button"
                        variant="secondary"
                        disabled={expired}
                        onClick={props.onEdit}
                    >
                        <Pencil className="h-4 w-4" />
                        Edit
                    </Button>
                    <Button
                        tooltip="Replace the secret and invalidate the current one"
                        type="button"
                        variant="secondary"
                        disabled={expired}
                        onClick={props.onRotate}
                    >
                        <RotateCw className="h-4 w-4" />
                        Rotate
                    </Button>
                    <Button
                        tooltip="Delete the token and reject its clients"
                        type="button"
                        variant="danger"
                        onClick={props.onDelete}
                    >
                        <Trash2 className="h-4 w-4" />
                        Delete
                    </Button>
                </div>
            </div>
        </article>
    );
}

/** Shares policy controls between creation and editing so permission descriptions stay consistent. */
function TokenDialog(
    props: {
        agents: Agents;
        onClose: () => void;
    } & (
        | { token?: undefined; onCreated: (secret: string) => Promise<void> }
        | { token: AccessTokenResponse; onUpdated: () => Promise<void> }
    ),
) {
    const { api } = Route.useRouteContext();
    const [name, setName] = React.useState(props.token?.name ?? "");
    const [expiry, setExpiry] = React.useState("90");

    const [permissions, setPermissions] = React.useState<Permission[]>(
        props.token?.permissions ?? ["files:read"],
    );

    const [deviceMode, setDeviceMode] = React.useState<DeviceScope["kind"]>(
        props.token?.devices.kind ?? "all",
    );

    const [deviceIds, setDeviceIds] = React.useState<string[]>(
        props.token?.devices.kind === "only"
            ? props.token.devices.device_ids
            : [],
    );

    const devices: DeviceScope =
        deviceMode === "all"
            ? { kind: "all" }
            : { kind: "only", device_ids: deviceIds };

    const canSubmit =
        name.trim().length > 0 &&
        permissions.length > 0 &&
        (deviceMode === "all" || deviceIds.length > 0);

    const mutation = useMutation({
        mutationFn: async () => {
            const policy = { name: name.trim(), permissions, devices };

            if (props.token) {
                await api.updateAccessToken(props.token.id, policy);
                await props.onUpdated();
            } else {
                const result = await api.createAccessToken({
                    ...policy,
                    expires_at:
                        expiry === "never"
                            ? null
                            : new Date(
                                  Date.now() + Number(expiry) * 86_400_000,
                              ).toISOString(),
                });

                await props.onCreated(result.token);
            }
        },
    });

    /** Keeps the dialog open during saving so failures remain visible beside the draft. */
    const close = () => {
        if (!mutation.isPending) props.onClose();
    };

    return (
        <Dialog
            isOpen
            title={props.token ? "Edit access token" : "Create access token"}
            description={
                props.token
                    ? "Changes apply to future requests using the same secret. Choose the least access this client needs."
                    : "Choose the least access this client needs."
            }
            closeAriaLabel={
                props.token ? "Close token editing" : "Close token creation"
            }
            isBusy={mutation.isPending}
            errorMessage={
                mutation.isError
                    ? getErrorMessage(
                          mutation.error,
                          "Could not save the token",
                      )
                    : null
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
                {props.token ? null : (
                    <label className="text-sm font-medium text-slate-300">
                        Expiry
                        <Select
                            aria-label="Token expiry"
                            value={expiry}
                            disabled={mutation.isPending}
                            containerClassName="mt-2 block"
                            onChange={(event) => setExpiry(event.target.value)}
                        >
                            <option value="30">30 days</option>
                            <option value="90">90 days</option>
                            <option value="365">365 days</option>
                            <option value="never">Never</option>
                        </Select>
                        <span className="mt-1.5 block text-xs font-normal text-slate-500">
                            Short expiries limit exposure if a client leaks its
                            token.
                        </span>
                    </label>
                )}
                <PermissionFieldset
                    permissions={permissions}
                    disabled={mutation.isPending}
                    onChange={setPermissions}
                />
                <DeviceScopeFields
                    agents={props.agents}
                    mode={deviceMode}
                    deviceIds={deviceIds}
                    disabled={mutation.isPending}
                    onModeChange={setDeviceMode}
                    onDeviceIdsChange={setDeviceIds}
                />
            </div>
            <DialogActions stackOnMobile>
                <Button
                    tooltip="Discard changes"
                    type="button"
                    variant="secondary"
                    disabled={mutation.isPending}
                    onClick={close}
                >
                    Cancel
                </Button>
                <Button
                    tooltip={
                        canSubmit
                            ? "Save the token policy"
                            : "Enter a name, at least one permission, and at least one device"
                    }
                    type="button"
                    isLoading={mutation.isPending}
                    disabled={!canSubmit}
                    onClick={() => mutation.mutate()}
                >
                    {props.token ? "Save changes" : "Create token"}
                </Button>
            </DialogActions>
        </Dialog>
    );
}

/** Renders permissions from the exhaustive label table so the order and warnings match the token cards. */
function PermissionFieldset(props: {
    permissions: Permission[];
    disabled: boolean;
    onChange: (permissions: Permission[]) => void;
}) {
    return (
        <fieldset className="space-y-2">
            <legend className="mb-2 text-sm font-medium text-slate-300">
                Permissions
            </legend>
            {permissionOrder.map((permission) => (
                <Checkbox
                    key={permission}
                    role="checkbox"
                    checked={props.permissions.includes(permission)}
                    disabled={props.disabled}
                    onCheckedChange={(checked) =>
                        props.onChange(
                            checked
                                ? [...props.permissions, permission]
                                : props.permissions.filter(
                                      (item) => item !== permission,
                                  ),
                        )
                    }
                >
                    <span>
                        <span className="block text-left">
                            {permissionLabels[permission].label}
                        </span>
                        <span
                            className={
                                permission === "commands:execute"
                                    ? "block max-w-sm text-left text-xs text-amber-300"
                                    : "block max-w-sm text-left text-xs text-slate-500"
                            }
                        >
                            {permissionLabels[permission].description}
                        </span>
                    </span>
                </Checkbox>
            ))}
        </fieldset>
    );
}

/** Separates "all devices" from an explicit list because only the former also covers devices added later. */
function DeviceScopeFields(props: {
    agents: Agents;
    mode: DeviceScope["kind"];
    deviceIds: string[];
    disabled: boolean;
    onModeChange: (mode: DeviceScope["kind"]) => void;
    onDeviceIdsChange: (deviceIds: string[]) => void;
}) {
    return (
        <div className="space-y-3">
            <RadioCardGroup
                legend="Allowed devices"
                legendClassName="mb-2 text-sm font-medium text-slate-300"
                disabled={props.disabled}
            >
                <RadioCardOption
                    name="token-device-scope"
                    value="all"
                    label="All devices"
                    description="Including devices added later"
                    checked={props.mode === "all"}
                    layout="descriptive"
                    onChange={() => props.onModeChange("all")}
                />
                <RadioCardOption
                    name="token-device-scope"
                    value="only"
                    label="Selected devices"
                    description="Only the devices checked below"
                    checked={props.mode === "only"}
                    layout="descriptive"
                    onChange={() => props.onModeChange("only")}
                />
            </RadioCardGroup>
            {props.mode === "only" ? (
                <fieldset className="space-y-1 pl-1">
                    <legend className="sr-only">Selected devices</legend>
                    {props.agents.length === 0 ? (
                        <p className="text-sm text-slate-500">
                            No configured devices are available.
                        </p>
                    ) : (
                        props.agents.map((agent) => (
                            <Checkbox
                                key={agent.id}
                                role="checkbox"
                                checked={props.deviceIds.includes(agent.id)}
                                disabled={props.disabled}
                                onCheckedChange={(checked) =>
                                    props.onDeviceIdsChange(
                                        checked
                                            ? [...props.deviceIds, agent.id]
                                            : props.deviceIds.filter(
                                                  (id) => id !== agent.id,
                                              ),
                                    )
                                }
                            >
                                {agent.name}
                            </Checkbox>
                        ))
                    )}
                    {props.deviceIds.length === 0 ? (
                        <p className="text-xs text-amber-300">
                            Select at least one device.
                        </p>
                    ) : null}
                </fieldset>
            ) : null}
        </div>
    );
}

/** Displays the sole recoverable copy of a new secret until the user closes the workflow. */
function SecretDialog(props: {
    secret: { value: string; action: "created" | "rotated" } | null;
    origin: string;
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
            description="Copy this token now. Redoor stores only a verifier and cannot show it again."
            closeAriaLabel="Close one-time token"
            onClose={props.onClose}
        >
            <div className="mt-5 space-y-4">
                <CopyableCodeRow
                    label="Access token"
                    value={props.secret.value}
                    copyAriaLabel="Copy access token"
                />
                <CopyableCodeRow
                    label="MCP endpoint"
                    value={`${props.origin}/mcp`}
                    copyAriaLabel="Copy MCP endpoint"
                />
                <p className="text-sm text-slate-400">
                    MCP clients should POST to this endpoint with{" "}
                    <code className="text-slate-200">
                        Authorization: Bearer &lt;token&gt;
                    </code>{" "}
                    and protocol version{" "}
                    <code className="text-slate-200">2026-07-28</code>.
                </p>
                <p className="text-sm text-slate-400">
                    For the remote CLI, run{" "}
                    <code className="text-slate-200">
                        redoor remote login {props.origin}
                    </code>{" "}
                    instead of pasting a token; it creates its own.
                </p>
            </div>
            <DialogActions>
                <Button
                    tooltip="Close and permanently hide this token"
                    type="button"
                    onClick={props.onClose}
                >
                    I saved the token
                </Button>
            </DialogActions>
        </Dialog>
    );
}
