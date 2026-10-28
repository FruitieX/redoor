import { describe, expect, test } from "vitest";
import {
    archiveCreateFormats,
    buildCreateArchiveCommand,
    buildUnarchiveCommand,
    getArchiveInfo,
    getArchiveNameError,
    getCreateArchiveSelectionError,
    getCustomArchiveDirectoryError,
    getDefaultArchiveName,
    quoteShellArgument,
} from "#ui/utils/archive";

describe("archive extraction command policy", () => {
    test.each([
        ["bundle.zip", "zip"],
        ["bundle.tar", "tar"],
        ["bundle.tar.gz", "tar-gzip"],
        ["bundle.tgz", "tar-gzip"],
        ["bundle.tar.bz2", "tar-bzip2"],
        ["bundle.tbz", "tar-bzip2"],
        ["bundle.tbz2", "tar-bzip2"],
        ["bundle.tar.xz", "tar-xz"],
        ["bundle.txz", "tar-xz"],
        ["bundle.tar.zst", "tar-zstd"],
        ["bundle.tzst", "tar-zstd"],
        ["bundle.rar", "rar"],
        ["bundle.7z", "7z"],
    ] as const)("recognizes %s as %s", (fileName, format) => {
        // Every documented extension and alias must select its intended extractor policy.
        expect(getArchiveInfo(fileName)?.format).toBe(format);
    });

    test("uses the longest suffix and preserves case-insensitive stems", () => {
        // Compound suffix removal must not leave a misleading trailing .tar destination.
        expect(getArchiveInfo("Release.TAR.GZ")).toEqual({
            format: "tar-gzip",
            suffix: ".TAR.GZ",
            directoryName: "Release",
        });
    });

    test("rejects unsupported filenames", () => {
        // Ordinary files must never expose an extraction command by suffix coincidence.
        expect(getArchiveInfo("notes.txt")).toBeNull();
        // A trailing backup suffix means the file is not one of the supported archive types.
        expect(getArchiveInfo("bundle.zip.old")).toBeNull();
    });

    test.each([
        "bad\nname.zip",
        "bad\rname.zip",
        "bad\u001bname.zip",
        "bad\u007fname.zip",
    ])("rejects terminal control characters in %s", (fileName) => {
        // PTY control bytes can affect the terminal before the shell applies argument quoting.
        expect(getArchiveInfo(fileName)).toBeNull();
        expect(buildUnarchiveCommand(fileName, "current")).toBeNull();
    });

    test.each([".zip", "..zip", "...zip"])(
        "uses a safe fallback directory for %s",
        (fileName) => {
            // Empty and dot-only stems must not target the current or parent directory.
            expect(getArchiveInfo(fileName)?.directoryName).toBe("archive");
            expect(buildUnarchiveCommand(fileName, "subdirectory")).toContain(
                "mkdir -- './archive'",
            );
        },
    );

    test("quotes hostile shell arguments", () => {
        // Single quotes and metacharacters must remain literal data in the generated shell input.
        expect(quoteShellArgument("a 'quote' $(touch nope)")).toBe(
            `'a '"'"'quote'"'"' $(touch nope)'`,
        );
    });

    test.each([
        ["a.zip", "unzip './a.zip'"],
        ["a.tar", "tar -xf './a.tar'"],
        ["a.tgz", "tar -xzf './a.tgz'"],
        ["a.tbz2", "tar -xjf './a.tbz2'"],
        ["a.txz", "tar -xJf './a.txz'"],
        ["a.tar.zst", "tar --zstd -xf './a.tar.zst'"],
        ["a.rar", "unrar x './a.rar'"],
        ["a.7z", "7z x './a.7z'"],
    ] as const)(
        "builds the current-directory command for %s",
        (name, command) => {
            // Current extraction must invoke the format-specific tool without inventing a destination.
            expect(buildUnarchiveCommand(name, "current")).toBe(command);
        },
    );

    test("builds safely quoted subdirectory commands", () => {
        // Prefixing local paths protects option-like names while quoting protects shell syntax.
        expect(
            buildUnarchiveCommand("-stuff's $(bad).zip", "subdirectory"),
        ).toBe(
            `mkdir -- './-stuff'"'"'s $(bad)' && unzip './-stuff'"'"'s $(bad).zip' -d './-stuff'"'"'s $(bad)'`,
        );
        // Tar extraction must create the destination before using its -C option.
        expect(buildUnarchiveCommand("stuff.tar.gz", "subdirectory")).toBe(
            "mkdir -- './stuff' && tar -xzf './stuff.tar.gz' -C './stuff'",
        );
        // 7z requires its output directory to remain attached to the -o option.
        expect(buildUnarchiveCommand("stuff.7z", "subdirectory")).toBe(
            "mkdir -- './stuff' && 7z x './stuff.7z' '-o./stuff'",
        );
    });

    test("builds custom destination commands with normalized hostile names", () => {
        // Custom and automatic destinations must share literal shell argument protection.
        expect(
            buildUnarchiveCommand(
                "stuff.tar.gz",
                "custom",
                "  -custom's $(bad)  ",
            ),
        ).toBe(
            `mkdir -- './-custom'"'"'s $(bad)' && tar -xzf './stuff.tar.gz' -C './-custom'"'"'s $(bad)'`,
        );
        // 7z needs safe custom output names while keeping its attached output option.
        expect(buildUnarchiveCommand("stuff.7z", "custom", "my output")).toBe(
            "mkdir -- './my output' && 7z x './stuff.7z' '-o./my output'",
        );
    });

    test.each([
        ["", "Directory name is required"],
        ["   ", "Directory name is required"],
        [".", "Directory name must not be . or .."],
        ["..", "Directory name must not be . or .."],
        [
            "nested/name",
            "Directory name must be a single path component without path separators",
        ],
        [
            "nested\\name",
            "Directory name must be a single path component without path separators",
        ],
        [
            "bad\u001bname",
            "Directory name cannot contain terminal control characters",
        ],
        [
            "bad\u009bname",
            "Directory name cannot contain terminal control characters",
        ],
    ])("rejects unsafe custom destination %j", (name, message) => {
        // Invalid values cannot escape the containing directory or inject terminal behavior.
        expect(getCustomArchiveDirectoryError(name)).toBe(message);
        expect(buildUnarchiveCommand("stuff.zip", "custom", name)).toBeNull();
    });
});

describe("archive creation command policy", () => {
    const agentId = "agent-a";

    test("lists formats from fastest modern default to most compatible", () => {
        // The dialog order and its default come straight from this list.
        expect(archiveCreateFormats.map((entry) => entry.extension)).toEqual([
            ".tar.zst",
            ".tar.xz",
            ".tar.gz",
            ".zip",
        ]);
    });

    test.each(archiveCreateFormats.map((entry) => entry.extension))(
        "creates %s archives that unarchive recognizes",
        (extension) => {
            // Every archive the UI creates must also be offered for extraction afterwards.
            expect(getArchiveInfo(`bundle${extension}`)).not.toBeNull();
        },
    );

    test.each([
        ["tar-zstd", "tar --zstd -cf './out.tar.zst' './a.txt' './b'"],
        ["tar-xz", "tar -cJf './out.tar.xz' './a.txt' './b'"],
        ["tar-gzip", "tar -czf './out.tar.gz' './a.txt' './b'"],
        ["zip", "zip -r './out.zip' './a.txt' './b'"],
    ] as const)("builds a same-directory %s command", (format, command) => {
        const extension =
            archiveCreateFormats.find((entry) => entry.format === format)
                ?.extension ?? "";

        // Sources beside the archive are added by name without any directory switching.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources: [
                    { agentId, path: "/work/a.txt" },
                    { agentId, path: "/work/b" },
                ],
                directoryPath: "/work",
                archiveName: `out${extension}`,
                format,
            }),
        ).toBe(command);
    });

    test("switches tar directories so sources keep only their own names", () => {
        // -C lets one tar invocation read from several parents while storing short names.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources: [
                    { agentId, path: "/work/a.txt" },
                    { agentId, path: "/other/b" },
                    { agentId, path: "/work/c" },
                ],
                directoryPath: "/work",
                archiveName: "out.tar.gz",
                format: "tar-gzip",
            }),
        ).toBe("tar -czf './out.tar.gz' './a.txt' './c' -C '/other' './b'");
    });

    test("changes zip directories and returns to the viewed directory", () => {
        // zip has no -C, so it needs an absolute archive path and a final cd back.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources: [
                    { agentId, path: "/other/b" },
                    { agentId, path: "/work/a.txt" },
                ],
                directoryPath: "/work",
                archiveName: "out.zip",
                format: "zip",
            }),
        ).toBe(
            "cd '/other' && zip -r '/work/out.zip' './b' && cd '/work' && zip -r '/work/out.zip' './a.txt'",
        );
        // Ending in another directory must leave the terminal where the user opened it.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources: [{ agentId, path: "/other/b" }],
                directoryPath: "/work",
                archiveName: "out.zip",
                format: "zip",
            }),
        ).toBe("cd '/other' && zip -r '/work/out.zip' './b' && cd '/work'");
    });

    test("quotes hostile source and archive names", () => {
        // Option-like and shell-special names must stay literal arguments.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources: [{ agentId, path: "/work/-it's $(bad)" }],
                directoryPath: "/work",
                archiveName: " -out's.tar.gz ",
                format: "tar-gzip",
            }),
        ).toBe(`tar -czf './-out'"'"'s.tar.gz' './-it'"'"'s $(bad)'`);
    });

    test.each([
        [
            [
                { agentId, path: "/work/a" },
                { agentId: "agent-b", path: "/work/b" },
            ],
            "Archives cannot be created when items are selected from multiple devices",
        ],
        [
            [{ agentId: "agent-b", path: "/work/a" }],
            "Archives are created in the current directory, so the selected items must be on this device",
        ],
        [[{ agentId, path: "/" }], "The filesystem root cannot be archived"],
        [
            [{ agentId, path: "/work/bad\u001bname" }],
            "Archived paths cannot contain terminal control characters",
        ],
        [
            [{ agentId, path: "/work" }],
            "Archives cannot be created inside a selected directory",
        ],
        [
            [
                { agentId, path: "/work/a/x" },
                { agentId, path: "/work/b/x" },
            ],
            "Several selected items are named x, so they cannot be stored side by side",
        ],
        [[], "Select items to archive"],
    ])("refuses selection %j", (sources, message) => {
        // Each refusal is shown as the disabled button's tooltip.
        expect(
            getCreateArchiveSelectionError({
                sources,
                agentId,
                directoryPath: "/work",
            }),
        ).toBe(message);
        // Unsafe selections must never produce a runnable command.
        expect(
            buildCreateArchiveCommand({
                agentId,
                sources,
                directoryPath: "/work",
                archiveName: "out.zip",
                format: "zip",
            }),
        ).toBeNull();
    });

    test("names single items after themselves and several items generically", () => {
        // A lone item keeps its name; a mixed selection has no obvious one.
        expect(getDefaultArchiveName([{ agentId, path: "/work/notes" }])).toBe(
            "notes",
        );
        expect(
            getDefaultArchiveName([
                { agentId, path: "/work/a" },
                { agentId, path: "/work/b" },
            ]),
        ).toBe("archive");
    });

    test("rejects unsafe archive names", () => {
        // Archive names share the single-component rules of extraction targets.
        expect(getArchiveNameError("nested/out")).toBe(
            "Archive name must be a single path component without path separators",
        );
        expect(getArchiveNameError("  ")).toBe("Archive name is required");
    });
});
