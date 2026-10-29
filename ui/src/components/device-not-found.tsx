import { HardDrive, SearchX } from "lucide-react";

import { LinkButton } from "#ui/components/link-button";
import { Tooltip } from "#ui/components/tooltip";

/**
 * Explains a device URL that points at no device. Device URLs carry the name, so an old tab,
 * bookmark or shared link stops working after a rename or removal; a standalone agent that has
 * not connected since the server restarted is also unknown until it does. The link to the
 * device list is the way back in every one of these cases.
 */
export function DeviceNotFound(props: { deviceName: string }) {
    return (
        <div className="flex h-full items-center justify-center p-8">
            <section
                aria-labelledby="device-not-found-heading"
                className="max-w-lg text-center"
            >
                <SearchX
                    className="mx-auto h-12 w-12 text-amber-400"
                    aria-hidden="true"
                />
                <h1
                    id="device-not-found-heading"
                    className="mt-4 text-2xl font-semibold text-slate-100"
                >
                    Device not found
                </h1>
                <p className="mt-2 break-words text-slate-400">
                    No device is called{" "}
                    <span className="font-mono text-slate-200">
                        {props.deviceName}
                    </span>
                    . It may have been renamed or removed, or its agent has not
                    connected since the server started.
                </p>
                <Tooltip content="Open the list of all devices">
                    <LinkButton to="/agents" className="mt-6">
                        <HardDrive className="h-4 w-4" aria-hidden="true" />
                        All devices
                    </LinkButton>
                </Tooltip>
            </section>
        </div>
    );
}
