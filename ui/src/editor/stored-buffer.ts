import { queryOptions } from "@tanstack/react-query";
import { z } from "zod";
import type { ApiClient } from "#ui/api-client";
import type { EditorBufferResponse } from "#bindings/EditorBufferResponse";
import { queryKeys } from "#ui/queries";

/**
 * Where a buffer was scrolled, as a document position plus the offset of that line from the
 * top of the viewport. A pixel `scrollTop` would point at another line after a reload, since
 * line heights below the viewport are only estimates until they are drawn.
 */
export const storedScrollSchema = z.object({
    anchor: z.number().int().min(0),
    yMargin: z.number(),
});

export type StoredScroll = z.infer<typeof storedScrollSchema>;

/**
 * The editor's part of a stored buffer, which the server keeps as opaque JSON. `state` is
 * CodeMirror's own JSON (document, selection and undo history); `baseHash` identifies the
 * device text it is based on, so a reload can tell unsaved edits apart and notice that the
 * device changed meanwhile without storing that text a second time.
 */
export const storedBufferDataSchema = z.object({
    version: z.literal(1),
    // CodeMirror's JSON is only read back by CodeMirror, and may hold `undefined` fields that
    // serialization drops, so only its outer form is checked here.
    state: z.record(z.string(), z.unknown()),
    baseHash: z.string(),
    scroll: storedScrollSchema.nullable(),
    /** The history entry whose `?line=` target was already applied, so a reload skips it. */
    lineEntry: z.string().nullable(),
    /**
     * The id of the open files list's line request already applied, so it is shown once. A
     * buffer stored before such requests existed has none.
     */
    revealEntry: z.string().nullable().catch(null),
    /** Lets the open files list ask before closing a file that was not visited since a reload. */
    dirty: z.boolean(),
});

export type StoredBufferData = z.infer<typeof storedBufferDataSchema>;

/** A stored buffer with the server version of the write that stored it. */
export type StoredBuffer = {
    version: number;
    data: StoredBufferData;
};

/**
 * How large a stored buffer may get, below the server's 8 MiB cap so the JSON envelope fits.
 * A buffer above it is stored without its undo history, and not at all if still too large.
 */
export const MAX_STORED_BUFFER_CHARS = 7 * 1024 * 1024;

/**
 * Validates the opaque server payload at the boundary. A buffer written by an incompatible
 * editor version is treated as absent, so the file simply opens from the device.
 */
export function parseStoredBuffer(
    response: EditorBufferResponse,
): StoredBuffer | null {
    if (response.buffer === null) {
        return null;
    }

    const parsed = storedBufferDataSchema.safeParse(response.buffer.data);

    return parsed.success
        ? { version: response.buffer.version, data: parsed.data }
        : null;
}

/**
 * Reads a file's stored buffer. It goes stale immediately, so the server is asked again when
 * the file is shown and when the window is focused or becomes visible: that is how a tab
 * picks up where the user left off on another computer.
 */
export function editorBufferQueryOptions(api: ApiClient, key: string) {
    return queryOptions({
        queryKey: queryKeys.editorBuffer(key),
        queryFn: async () => parseStoredBuffer(await api.getEditorBuffer(key)),
        staleTime: 0,
        // A failed read must not hold the editor back; the file then opens from the device.
        retry: false,
    });
}
