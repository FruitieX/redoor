import * as React from "react";
import { getRouteApi } from "@tanstack/react-router";

import type { UploadQueue, UploadQueueSnapshot } from "#ui/upload-queue";

const rootRouteApi = getRouteApi("__root__");

/** Returns the application's upload queue, which outlives every route and component. */
export function useUploads(): UploadQueue {
    return rootRouteApi.useRouteContext().uploads;
}

/** Subscribes to the rows of the current upload batch. */
export function useUploadQueueSnapshot(): UploadQueueSnapshot {
    const uploads = useUploads();

    return React.useSyncExternalStore(uploads.subscribe, uploads.getSnapshot);
}
