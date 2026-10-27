import { expect, test } from "vitest";

import { FileNameFilter } from "#ui/utils/file-name-filter";

const paths = [
    "src/server.ts",
    "src/server.test.ts",
    "src/my-test/helpers.ts",
    "docs/Read Me.md",
];

/** The paths a query keeps, so each case reads as query in, file list out. */
function filtered(query: string) {
    const filter = FileNameFilter.parse(query);

    return paths.filter((path) => filter.matches(path));
}

test("keeps paths containing every word, ignoring case", () => {
    // Words narrow the list together, wherever they appear in the path.
    expect(filtered("SRC server")).toEqual([
        "src/server.ts",
        "src/server.test.ts",
    ]);
});

test("drops paths containing a dashed word", () => {
    // `-test` hides every test file, including the directory that only contains "test".
    expect(filtered("-test")).toEqual(["src/server.ts", "docs/Read Me.md"]);
    // Includes and excludes combine.
    expect(filtered("src -helpers")).toEqual([
        "src/server.ts",
        "src/server.test.ts",
    ]);
});

test("keeps quoted words literal", () => {
    // A quoted dash is part of the word instead of negating it.
    expect(filtered('"-test"')).toEqual(["src/my-test/helpers.ts"]);
    // Quotes keep a space inside one word.
    expect(filtered('"read me"')).toEqual(["docs/Read Me.md"]);
    // A dash before quotes negates the quoted word.
    expect(filtered('-"-test"')).toEqual([
        "src/server.ts",
        "src/server.test.ts",
        "docs/Read Me.md",
    ]);
});

test("ignores words that are still being typed", () => {
    // A lone dash, empty quotes and blank queries filter nothing, so the list does not jump.
    expect(FileNameFilter.parse("-").isActive).toBe(false);
    expect(FileNameFilter.parse('""').isActive).toBe(false);
    expect(FileNameFilter.parse("   ").isActive).toBe(false);
    // An unclosed quote still filters by what was typed after it.
    expect(filtered('"my-')).toEqual(["src/my-test/helpers.ts"]);
});
