import { describe, expect, test } from "vitest";

import {
    expandHomePath,
    getBaseName,
    validatePathComponent,
} from "#ui/utils/path";

describe("validatePathComponent", () => {
    test("accepts an ordinary name", () => {
        // A plain name is the normal case every naming dialog must allow.
        expect(validatePathComponent("notes.txt", "File")).toBeNull();
        // Dots inside or at the start of a name are legitimate hidden or extension names.
        expect(validatePathComponent(".env.local", "File")).toBeNull();
        expect(validatePathComponent("...", "Directory")).toBeNull();
    });

    test("requires a name", () => {
        // An empty name would otherwise resolve to the parent directory itself.
        expect(validatePathComponent("", "File")).toBe("File name is required");
        expect(validatePathComponent("", "Directory")).toBe(
            "Directory name is required",
        );
    });

    test("rejects separators when only one component is allowed", () => {
        // A slash would place the entry somewhere other than the directory the dialog was opened in.
        expect(validatePathComponent("a/b", "File")).toBe(
            "File name must be a single path component",
        );
        expect(validatePathComponent("/abs", "Directory")).toBe(
            "Directory name must be a single path component",
        );
    });

    test("rejects the current and parent directory names", () => {
        // These resolve to existing directories rather than creating or renaming to a new entry.
        expect(validatePathComponent(".", "File")).toBe(
            'File name cannot contain "." or ".." components',
        );
        expect(validatePathComponent("..", "Directory")).toBe(
            'Directory name cannot contain "." or ".." components',
        );
    });

    test("accepts nested names only when asked and validates every component", () => {
        // Directory creation makes missing parents on the agent, so nesting is an explicit opt-in.
        expect(
            validatePathComponent("a/b/c", "Directory", { allowNested: true }),
        ).toBeNull();
        // Without the opt-in the same name is refused.
        expect(validatePathComponent("a/b/c", "Directory")).not.toBeNull();
        // A parent reference inside a nested name would still escape the directory.
        expect(
            validatePathComponent("a/../b", "Directory", { allowNested: true }),
        ).toBe('Directory name cannot contain "." or ".." components');
        // Empty components come from leading, trailing or doubled slashes.
        expect(
            validatePathComponent("a//b", "Directory", { allowNested: true }),
        ).toBe("Directory name cannot have empty path components");
        expect(
            validatePathComponent("/a", "Directory", { allowNested: true }),
        ).toBe("Directory name cannot have empty path components");
    });
});

describe("expandHomePath", () => {
    test("expands the home shorthand against the agent home", () => {
        // A bare tilde is the home directory itself.
        expect(expandHomePath("/home/ada", "~")).toBe("/home/ada");
        // A tilde prefix is resolved below the home directory.
        expect(expandHomePath("/home/ada", "~/src")).toBe("/home/ada/src");
        // A trailing slash on the home must not double up.
        expect(expandHomePath("/home/ada/", "~/src")).toBe("/home/ada/src");
    });

    test("handles a root home without a double slash", () => {
        // A root home would otherwise produce "//src", which is a different path on some systems.
        expect(expandHomePath("/", "~/src")).toBe("/src");
    });

    test("leaves other paths and unknown homes untouched", () => {
        // Absolute paths never need the home.
        expect(expandHomePath("/home/ada", "/etc")).toBe("/etc");
        // A name that merely starts with a tilde is not the shorthand.
        expect(expandHomePath("/home/ada", "~other")).toBe("~other");
        // Without a known home the shorthand cannot be expanded.
        expect(expandHomePath(null, "~/src")).toBe("~/src");
    });
});

describe("getBaseName", () => {
    test("returns the last non-empty segment", () => {
        // The base name titles views and names archives.
        expect(getBaseName("/home/ada/notes.txt")).toBe("notes.txt");
        // A trailing slash must not produce an empty name.
        expect(getBaseName("/home/ada/")).toBe("ada");
    });

    test("names the filesystem root", () => {
        // The root has no segment, so views fall back to the slash itself.
        expect(getBaseName("/")).toBe("/");
    });
});
