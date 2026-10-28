import { expect, test } from "vitest";

import {
    commitDeviceOrder,
    defaultUserState,
    openFileAtLine,
    reorderBookmarks,
    reorderOpenFiles,
    resolveDeviceOrder,
    userStateSchema,
    type Bookmark,
    type OpenFile,
} from "#ui/user-state";

/** Supplies metadata so movement tests also exercise object preservation. */
function openEntry(agentId: string, name: string): OpenFile {
    return { agentId, path: `/${name}`, name, agentName: agentId };
}

/** Gives each device a distinct path to prove that scoped moves retain ownership. */
function bookmark(agentId: string, name: string): Bookmark {
    return {
        agentId,
        path: `/${agentId}/${name}`,
        name,
        entryType: "file",
    };
}

test("old and malformed documents fall back without dropping other defaults", () => {
    const missing = userStateSchema.parse({ showHiddenFiles: false });
    // A document without a device order keeps the empty default instead of failing to load.
    expect(missing.deviceOrder).toEqual([]);
    // Saved documents from before the device filter must continue to show all devices.
    expect(missing.showOnlyConnectedDevices).toBe(false);
    expect(missing.showHiddenFiles).toBe(false);
    expect(missing.theme).toBe(defaultUserState.theme);

    expect(userStateSchema.parse({ deviceOrder: "nope" }).deviceOrder).toEqual(
        [],
    );
    expect(
        userStateSchema.parse({ deviceOrder: [1, "a"] }).deviceOrder,
    ).toEqual([]);
    expect(userStateSchema.safeParse({}).success).toBe(true);
});

test("resolves device order from saved ids, then unsaved inventory order", () => {
    expect(resolveDeviceOrder([], ["a", "b", "c"])).toEqual(["a", "b", "c"]);
    expect(
        resolveDeviceOrder(["b", "b", "missing", "a"], ["a", "b", "c"]),
    ).toEqual(["b", "a", "c"]);
    // A returning id uses its remembered slot instead of appending as new.
    expect(
        resolveDeviceOrder(["a", "missing", "b"], ["a", "b", "missing"]),
    ).toEqual(["a", "missing", "b"]);
});

test("keeps unavailable device ids when the visible order changes", () => {
    const saved = ["a", "missing", "b"];

    const next = commitDeviceOrder(saved, ["a", "b", "c"], {
        activeId: "c",
        overId: "a",
    });

    // The missing device stays between the moved visible ids.
    expect(next).toEqual(["c", "missing", "a", "b"]);
    expect(resolveDeviceOrder(next, ["a", "b", "c"])).toEqual(["c", "a", "b"]);
});

test("does not normalize device order when the gesture changes nothing", () => {
    const saved = ["a", "missing"];

    expect(
        commitDeviceOrder(saved, ["a", "b", "c"], {
            activeId: "a",
            overId: "a",
        }),
    ).toBe(saved);
    expect(
        commitDeviceOrder(saved, ["a", "b"], {
            activeId: "gone",
            overId: "a",
        }),
    ).toBe(saved);
    expect(
        commitDeviceOrder([], ["only"], { activeId: "only", overId: "only" }),
    ).toEqual([]);
});

test("renames do not affect id-based device placement", () => {
    const saved = ["beta", "alpha"];
    // Display names are not part of the key, so a rename keeps the saved slots.
    expect(resolveDeviceOrder(saved, ["alpha", "beta"])).toEqual([
        "beta",
        "alpha",
    ]);
});

test("reorders open files and one bookmark scope against the latest arrays", () => {
    const files = [
        openEntry("a", "one"),
        openEntry("b", "two"),
        openEntry("a", "three"),
    ];

    const movedFiles = reorderOpenFiles(files, {
        activeId: "a:/three",
        overId: "a:/one",
    });

    expect(movedFiles.map((file) => file.name)).toEqual([
        "three",
        "one",
        "two",
    ]);
    expect(movedFiles[1]).toBe(files[0]);

    const bookmarks = [
        bookmark("a", "a1"),
        bookmark("b", "b1"),
        bookmark("a", "a2"),
        bookmark("b", "b2"),
        bookmark("a", "a3"),
    ];

    const moved = reorderBookmarks(bookmarks, "a", {
        activeId: "a:/a/a3",
        overId: "a:/a/a1",
    });

    expect(moved.map((entry) => entry.name)).toEqual([
        "a3",
        "b1",
        "a1",
        "b2",
        "a2",
    ]);
    // Equal names on another device stay in their slots.
    expect(
        moved
            .filter((entry) => entry.agentId === "b")
            .map((entry) => entry.path),
    ).toEqual(["/b/b1", "/b/b2"]);
});

test("a stale open file move cannot restore a closed file or discard a new one", () => {
    const current = [
        openEntry("a", "one"),
        openEntry("a", "three"),
        openEntry("b", "fresh"),
    ];

    expect(
        reorderOpenFiles(current, {
            activeId: "a:/two",
            overId: "a:/one",
        }),
    ).toBe(current);
    expect(
        reorderOpenFiles(current, {
            activeId: "a:/three",
            overId: "a:/one",
        }).map((file) => file.name),
    ).toEqual(["three", "one", "fresh"]);
});

test("opens a file at a line without moving an already open file", () => {
    const files = [openEntry("a", "one"), openEntry("a", "two")];
    const reveal = { line: 42, id: "r1" };

    const appended = openFileAtLine(files, openEntry("b", "three"), reveal);
    // A newly opened file goes last, carrying the line to show when it is visited.
    expect(appended.map((file) => file.name)).toEqual(["one", "two", "three"]);
    expect(appended[2]?.reveal).toEqual(reveal);

    const moved = openFileAtLine(files, openEntry("a", "one"), reveal);
    // An open file keeps its place and only gets the new line.
    expect(moved.map((file) => file.name)).toEqual(["one", "two"]);
    expect(moved[0]?.reveal).toEqual(reveal);
    // The other files are untouched, so React keeps their rows.
    expect(moved[1]).toBe(files[1]);

    // Without a line, opening an open file changes nothing, so no settings write follows.
    expect(openFileAtLine(files, openEntry("a", "two"), undefined)).toBe(files);
});

test("a malformed line request keeps the open file", () => {
    const parsed = userStateSchema.parse({
        openFiles: [{ ...openEntry("a", "one"), reveal: { line: "x" } }],
    });

    // Only the line is lost; dropping the whole list would close every open file.
    expect(parsed.openFiles).toEqual([openEntry("a", "one")]);
});
