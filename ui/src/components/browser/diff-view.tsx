/*
 * The diff table: files, hunks, rows and skipped stretches.
 *
 * The Diff/Hunk/Line/SkipBlockRow structure and the inline modified rows are ported from the
 * MIT-licensed diff viewer by Fredrika Lindh (https://github.com/fredrikalindh/ui,
 * registry/ui/diff/index.tsx), with the app's highlighter, theme colours and editor links.
 */
import React from "react";
import type { Language } from "@codemirror/language";
import { ChevronsUpDown } from "lucide-react";
import type { browserRoute } from "#ui/browser-route";
import { HighlightedTokens } from "#ui/components/highlighted-tokens";
import { LinkButton } from "#ui/components/link-button";
import {
    lineSideText,
    type DiffFile,
    type DiffHunk,
    type DiffLine,
    type LineSegment,
    type SkipBlock,
} from "#ui/utils/diff-parse";
import {
    highlightLines,
    languageForFileName,
    useLoadedLanguage,
    type HighlightLanguageSource,
    type HighlightToken,
} from "#ui/utils/code-highlight";
import { cn } from "#ui/utils/cn";

/** Where the editor opens the diffed file, as built by `browserRoute`. */
export type DiffEditorRoute = ReturnType<typeof browserRoute>;

/** Each side's highlighted tokens by line number, so a row looks its tokens up directly. */
interface FileHighlight {
    old: Map<number, HighlightToken[]>;
    new: Map<number, HighlightToken[]>;
}

/**
 * Patches larger than this render without colours. Highlighting runs during render, and a
 * multi-megabyte patch would block the page for longer than the colours are worth.
 */
const MAX_HIGHLIGHTED_CHARACTERS = 1_000_000;

/** The name a file is shown and highlighted by: the new path, or the old one if deleted. */
export function diffFileName(file: DiffFile): string {
    return file.newPath ?? file.oldPath ?? "";
}

/**
 * Finds the file's first line when the patch includes it, so an extensionless script still
 * highlights by its shebang.
 */
function firstFileLine(file: DiffFile): string {
    for (const hunk of file.hunks) {
        if (hunk.type !== "hunk") {
            continue;
        }

        for (const line of hunk.lines) {
            if (line.newLineNumber === 1) {
                return lineSideText(line, "new");
            }

            if (line.oldLineNumber === 1) {
                return lineSideText(line, "old");
            }
        }
    }

    return "";
}

/** Picks the file's grammar the same way the editor does. */
function diffFileLanguage(file: DiffFile): HighlightLanguageSource {
    return languageForFileName(diffFileName(file), firstFileLine(file));
}

/**
 * Highlights one side of a hunk as a block, so strings and comments spanning lines keep
 * their colours, and files each line's tokens under its line number.
 */
function highlightHunkSide(
    texts: Array<string | undefined>,
    start: number,
    language: Language,
): Array<[number, HighlightToken[]]> {
    if (texts.length === 0) {
        return [];
    }

    const lines = highlightLines(
        texts.map((text) => text ?? "").join("\n"),
        language,
    );

    const entries: Array<[number, HighlightToken[]]> = [];

    texts.forEach((text, index) => {
        const tokens = lines[index];

        // Tokens that do not spell the line exactly would cut inline segments wrongly.
        if (
            text === undefined ||
            tokens === undefined ||
            tokens.map((token) => token.text).join("") !== text
        ) {
            return;
        }

        entries.push([start + index, tokens]);
    });

    return entries;
}

/** Highlights both sides of every hunk of a file. */
function highlightFile(
    file: DiffFile,
    language: Language | null,
): FileHighlight | null {
    if (language === null) {
        return null;
    }

    const highlight: FileHighlight = { old: new Map(), new: new Map() };
    let characters = 0;

    for (const hunk of file.hunks) {
        if (hunk.type !== "hunk") {
            continue;
        }

        const oldTexts = Array.from<string | undefined>({
            length: hunk.oldLines,
        });

        const newTexts = Array.from<string | undefined>({
            length: hunk.newLines,
        });

        for (const line of hunk.lines) {
            if (line.oldLineNumber !== null) {
                const text = lineSideText(line, "old");
                oldTexts[line.oldLineNumber - hunk.oldStart] = text;
                characters += text.length;
            }

            if (line.newLineNumber !== null) {
                const text = lineSideText(line, "new");
                newTexts[line.newLineNumber - hunk.newStart] = text;
                characters += text.length;
            }
        }

        if (characters > MAX_HIGHLIGHTED_CHARACTERS) {
            return null;
        }

        for (const [line, tokens] of highlightHunkSide(
            oldTexts,
            hunk.oldStart,
            language,
        )) {
            highlight.old.set(line, tokens);
        }

        for (const [line, tokens] of highlightHunkSide(
            newTexts,
            hunk.newStart,
            language,
        )) {
            highlight.new.set(line, tokens);
        }
    }

    return highlight;
}

/** Cuts the tokens covering characters `start` to `end` of their line. */
function sliceTokens(
    tokens: HighlightToken[],
    start: number,
    end: number,
): HighlightToken[] {
    const result: HighlightToken[] = [];
    let position = 0;

    for (const token of tokens) {
        const tokenEnd = position + token.text.length;

        if (tokenEnd > start && position < end) {
            result.push({
                text: token.text.slice(
                    Math.max(0, start - position),
                    Math.min(token.text.length, end - position),
                ),
                className: token.className,
                kind: token.kind,
            });
        }

        position = tokenEnd;

        if (position >= end) {
            break;
        }
    }

    return result;
}

/** One inline segment of a row with its highlighted tokens. */
interface RowPart {
    type: LineSegment["type"];
    tokens: HighlightToken[];
}

/**
 * Splits a row's highlighted line into its inline segments. Each line is highlighted once
 * as a whole and then cut, rather than highlighting every segment alone, because a segment
 * such as half a string literal has no grammar of its own. Deleted text takes its colours
 * from the old line and inserted text from the new one; kept text advances through both.
 */
function rowParts(
    line: DiffLine,
    oldTokens: HighlightToken[] | undefined,
    newTokens: HighlightToken[] | undefined,
): RowPart[] {
    let oldOffset = 0;
    let newOffset = 0;

    return line.segments.map((segment) => {
        const fromOld =
            segment.type === "delete" ||
            (segment.type === "normal" && line.newLineNumber === null);

        const tokens = fromOld ? oldTokens : newTokens;
        const start = fromOld ? oldOffset : newOffset;
        const end = start + segment.value.length;

        if (segment.type !== "insert") {
            oldOffset += segment.value.length;
        }

        if (segment.type !== "delete") {
            newOffset += segment.value.length;
        }

        return {
            type: segment.type,
            tokens:
                tokens === undefined
                    ? [{ text: segment.value, className: null, kind: null }]
                    : sliceTokens(tokens, start, end),
        };
    });
}

/** Row backgrounds by kind; modified rows stay neutral so their inline changes stand out. */
const ROW_CLASSES = {
    normal: "bg-surface",
    insert: "bg-diff-insert",
    delete: "bg-diff-delete",
    modified: "bg-surface",
} satisfies Record<DiffLine["kind"], string>;

/** The left edge marks a changed row even where colours are hard to tell apart. */
const MARKER_CLASSES = {
    normal: "border-l-transparent",
    insert: "border-l-diff-insert-marker",
    delete: "border-l-diff-delete-marker",
    modified: "border-l-amber-400",
} satisfies Record<DiffLine["kind"], string>;

const SIGNS = {
    normal: "",
    insert: "+",
    delete: "-",
    modified: "~",
} satisfies Record<DiffLine["kind"], string>;

/**
 * The line numbers and change sign. It sticks to the left edge so the numbers stay in view
 * while a long line scrolls sideways, which is why its background must be opaque.
 */
function Gutter(props: {
    kind: DiffLine["kind"] | "skip";
    oldLineNumber: number | null;
    newLineNumber: number | null;
    numberWidth: number;
    children?: React.ReactNode;
}) {
    const kind = props.kind === "skip" ? "normal" : props.kind;

    return (
        <td
            className={cn(
                "sticky left-0 z-[1] select-none whitespace-nowrap border-l-3 px-2 text-right tabular-nums text-slate-500",
                props.kind === "skip" ? "bg-surface-raised" : ROW_CLASSES[kind],
                MARKER_CLASSES[kind],
            )}
        >
            {props.children ?? (
                <>
                    <span
                        className="inline-block"
                        style={{ minWidth: `${String(props.numberWidth)}ch` }}
                    >
                        {props.oldLineNumber ?? ""}
                    </span>
                    <span
                        className="ml-2 inline-block"
                        style={{ minWidth: `${String(props.numberWidth)}ch` }}
                    >
                        {props.newLineNumber ?? ""}
                    </span>
                    <span
                        aria-hidden="true"
                        className="ml-2 inline-block w-[1ch] text-center"
                    >
                        {SIGNS[kind]}
                    </span>
                </>
            )}
        </td>
    );
}

/**
 * One diff row. Whole-line inserts and deletes use `<ins>` and `<del>` so assistive
 * technology announces them; a modified row marks only its changed segments that way.
 * It is memoized because a large diff re-renders often and the rows themselves rarely change.
 */
const Line = React.memo(function Line(props: {
    line: DiffLine;
    oldTokens: HighlightToken[] | undefined;
    newTokens: HighlightToken[] | undefined;
    editorRoute: DiffEditorRoute | null;
    numberWidth: number;
}) {
    const line = props.line;
    const parts = rowParts(line, props.oldTokens, props.newTokens);

    const Tag =
        line.kind === "insert"
            ? "ins"
            : line.kind === "delete"
              ? "del"
              : "span";

    const openLine = line.newLineNumber;

    return (
        <>
            <tr
                data-line-kind={line.kind}
                data-line-old={line.oldLineNumber ?? undefined}
                data-line-new={line.newLineNumber ?? undefined}
                className={ROW_CLASSES[line.kind]}
            >
                <Gutter
                    kind={line.kind}
                    oldLineNumber={line.oldLineNumber}
                    newLineNumber={line.newLineNumber}
                    numberWidth={props.numberWidth}
                />
                <td className="w-full whitespace-pre pl-2 pr-4">
                    <Tag className="no-underline">
                        {parts.map((part, index) =>
                            part.type === "insert" ? (
                                <ins
                                    key={index}
                                    className="rounded-sm bg-diff-insert-strong no-underline"
                                >
                                    <HighlightedTokens tokens={part.tokens} />
                                </ins>
                            ) : part.type === "delete" ? (
                                <del
                                    key={index}
                                    className="rounded-sm bg-diff-delete-strong"
                                >
                                    <HighlightedTokens tokens={part.tokens} />
                                </del>
                            ) : (
                                <HighlightedTokens
                                    key={index}
                                    tokens={part.tokens}
                                />
                            ),
                        )}
                    </Tag>
                </td>
                <td className="select-none whitespace-nowrap py-px pr-2 text-right align-middle">
                    {props.editorRoute === null || openLine === null ? null : (
                        // Only new-side lines exist in the current file, so only they open.
                        <LinkButton
                            {...props.editorRoute}
                            search={{ line: openLine }}
                            variant="secondary"
                            size="sm"
                            aria-label={`Open line ${String(openLine)} in editor`}
                            className="px-1.5 py-0 font-sans text-[length:inherit] leading-[inherit]"
                        >
                            Open
                        </LinkButton>
                    )}
                </td>
            </tr>
            {line.missingNewline ? (
                <tr data-line-kind="no-newline" className="bg-surface">
                    <Gutter
                        kind="normal"
                        oldLineNumber={null}
                        newLineNumber={null}
                        numberWidth={props.numberWidth}
                    />
                    <td colSpan={2} className="pl-2 italic text-slate-500">
                        \ No newline at end of file
                    </td>
                </tr>
            ) : null}
        </>
    );
});

/** Stands in for the unchanged lines between hunks, with the section Git names there. */
export function SkipBlockRow(props: { block: SkipBlock; numberWidth: number }) {
    const hidden = `${String(props.block.count)} unchanged ${props.block.count === 1 ? "line" : "lines"} hidden`;

    return (
        <tr data-skip-block="" className="bg-surface-raised text-slate-400">
            <Gutter
                kind="skip"
                oldLineNumber={null}
                newLineNumber={null}
                numberWidth={props.numberWidth}
            >
                <ChevronsUpDown
                    className="mx-auto h-3.5 w-3.5 opacity-60"
                    aria-hidden="true"
                />
            </Gutter>
            <td colSpan={2} className="py-1 pl-2 italic">
                {props.block.content === ""
                    ? hidden
                    : `${hidden} · ${props.block.content}`}
            </td>
        </tr>
    );
}

/** One hunk's rows. */
export function Hunk(props: {
    hunk: DiffHunk;
    highlight: FileHighlight | null;
    editorRoute: DiffEditorRoute | null;
    numberWidth: number;
}) {
    return props.hunk.lines.map((line, index) => (
        <Line
            key={index}
            line={line}
            oldTokens={
                line.oldLineNumber === null
                    ? undefined
                    : props.highlight?.old.get(line.oldLineNumber)
            }
            newTokens={
                line.newLineNumber === null
                    ? undefined
                    : props.highlight?.new.get(line.newLineNumber)
            }
            editorRoute={props.editorRoute}
            numberWidth={props.numberWidth}
        />
    ));
}

/** Sizes the number columns for the file's largest line number, so every row aligns. */
function lineNumberWidth(file: DiffFile): number {
    let largest = 0;

    for (const hunk of file.hunks) {
        if (hunk.type === "hunk") {
            largest = Math.max(
                largest,
                hunk.oldStart + hunk.oldLines,
                hunk.newStart + hunk.newLines,
            );
        }
    }

    return String(largest).length;
}

/**
 * One file's rows as a table. It scrolls sideways inside its own box, so wide lines never
 * widen the page or the file header above them. Highlighting is memoized on the parsed file,
 * so re-rendering a large diff does not highlight it again.
 */
export function Diff(props: {
    file: DiffFile;
    label: string;
    editorRoute: DiffEditorRoute | null;
}) {
    const file = props.file;
    // A grammar that loads lazily renders plain rows first and colours them once loaded.
    const languageSource = React.useMemo(() => diffFileLanguage(file), [file]);
    const language = useLoadedLanguage(languageSource);

    const highlight = React.useMemo(
        () => highlightFile(file, language),
        [file, language],
    );

    const numberWidth = lineNumberWidth(file);

    return (
        <div className="w-full min-w-0 overflow-x-auto overflow-y-hidden">
            <table
                aria-label={props.label}
                className="syntax-highlight w-max min-w-full border-separate border-spacing-0 bg-surface font-mono text-code leading-5 text-slate-200 max-sm:leading-4"
            >
                <tbody>
                    {file.hunks.map((hunk, index) =>
                        hunk.type === "hunk" ? (
                            <Hunk
                                key={index}
                                hunk={hunk}
                                highlight={highlight}
                                editorRoute={props.editorRoute}
                                numberWidth={numberWidth}
                            />
                        ) : (
                            <SkipBlockRow
                                key={index}
                                block={hunk}
                                numberWidth={numberWidth}
                            />
                        ),
                    )}
                </tbody>
            </table>
        </div>
    );
}
