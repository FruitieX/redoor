import * as React from "react";
import {
    Eye,
    FilePen,
    FileX,
    type LucideIcon,
    Power,
    SquareTerminal,
} from "lucide-react";

import type { AccessTokenResponse } from "#bindings/AccessTokenResponse";
import type { DeviceScope } from "#bindings/DeviceScope";
import type { Permission } from "#bindings/Permission";
import { Checkbox } from "#ui/components/checkbox";
import { RadioCardGroup, RadioCardOption } from "#ui/components/radio-card";
import { TextField } from "#ui/components/text-field";

/**
 * Typed as an exhaustive record so a new server-side permission fails the TS
 * build until it gets a human label instead of silently showing a raw id.
 */
export const permissionLabels: Record<
    Permission,
    { label: string; description: string; icon: LucideIcon }
> = {
    "files:read": {
        label: "Read files",
        description: "List, search, read, and download files.",
        icon: Eye,
    },
    "files:write": {
        label: "Write files",
        description: "Upload, replace, copy, and create files or directories.",
        icon: FilePen,
    },
    "files:delete": {
        label: "Delete files",
        description: "Trash or permanently remove files and move sources.",
        icon: FileX,
    },
    "commands:execute": {
        label: "Run commands",
        description:
            "High trust: commands run as the agent's OS user and can bypass all file permissions.",
        icon: SquareTerminal,
    },
    "agents:manage": {
        label: "Manage agents",
        description: "Start and stop managed devices.",
        icon: Power,
    },
};

/** Fixed display order so the least dangerous permissions are listed first. */
export const permissionOrder: Permission[] = [
    "files:read",
    "files:write",
    "files:delete",
    "commands:execute",
    "agents:manage",
];

export type TokenAgents = Array<{ id: string; name: string }>;

/** Tokens past their expiry are rejected by the server, so the UI must not offer to extend their use. */
export function isExpired(token: AccessTokenResponse): boolean {
    return (
        token.expires_at !== null && Date.parse(token.expires_at) <= Date.now()
    );
}

/**
 * Editable policy state shared by the create dialog and the edit page, so both validate a
 * draft the same way before it reaches the server.
 */
export function useAccessTokenPolicyDraft(token: AccessTokenResponse | null) {
    const [name, setName] = React.useState(token?.name ?? "");

    const [permissions, setPermissions] = React.useState<Permission[]>(
        token?.permissions ?? ["files:read"],
    );

    const [deviceMode, setDeviceMode] = React.useState<DeviceScope["kind"]>(
        token?.devices.kind ?? "all",
    );

    const [deviceIds, setDeviceIds] = React.useState<string[]>(
        token?.devices.kind === "only" ? token.devices.device_ids : [],
    );

    const devices: DeviceScope =
        deviceMode === "all"
            ? { kind: "all" }
            : { kind: "only", device_ids: deviceIds };

    return {
        name,
        setName,
        permissions,
        setPermissions,
        deviceMode,
        setDeviceMode,
        deviceIds,
        setDeviceIds,
        policy: { name: name.trim(), permissions, devices },
        // An explicit device list with no devices would grant nothing, so it is rejected.
        canSubmit:
            name.trim().length > 0 &&
            permissions.length > 0 &&
            (deviceMode === "all" || deviceIds.length > 0),
    };
}

export type AccessTokenPolicyDraft = ReturnType<
    typeof useAccessTokenPolicyDraft
>;

/**
 * Renders the name, permission and device controls in one layout so creating and editing a
 * token always present the same choices. `extraField` sits beside the name, which keeps
 * create-only settings such as expiry out of the edit page without a second layout.
 */
export function AccessTokenPolicyFields(props: {
    draft: AccessTokenPolicyDraft;
    agents: TokenAgents;
    disabled: boolean;
    extraField?: React.ReactNode;
}) {
    return (
        <div className="grid gap-6 md:grid-cols-2">
            <div className={props.extraField ? undefined : "md:col-span-2"}>
                <TextField
                    label="Token name"
                    value={props.draft.name}
                    placeholder="Coding assistant"
                    description="Shown only in Redoor token management."
                    required
                    disabled={props.disabled}
                    onChange={props.draft.setName}
                />
            </div>
            {props.extraField}
            <PermissionFieldset
                permissions={props.draft.permissions}
                disabled={props.disabled}
                onChange={props.draft.setPermissions}
            />
            <DeviceScopeFields
                agents={props.agents}
                mode={props.draft.deviceMode}
                deviceIds={props.draft.deviceIds}
                disabled={props.disabled}
                onModeChange={props.draft.setDeviceMode}
                onDeviceIdsChange={props.draft.setDeviceIds}
            />
        </div>
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
    agents: TokenAgents;
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
