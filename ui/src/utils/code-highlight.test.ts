import { expect, test } from "vitest";
import {
    highlightLines,
    languageForFileName,
    languageForName,
    type HighlightToken,
} from "#ui/utils/code-highlight";
import { Language } from "@codemirror/language";

/** Highlights code with a bundled grammar, failing loudly when none is found. */
function highlight(fileName: string, code: string): HighlightToken[][] {
    const language = languageForFileName(fileName, code);

    if (!(language instanceof Language)) {
        throw new Error(`No bundled grammar for ${fileName}`);
    }

    return highlightLines(code, language);
}

/** The text of every token of a given kind, to assert what the grammar recognised. */
function tokensOfKind(lines: HighlightToken[][], kind: string): string[] {
    return lines
        .flat()
        .filter((token) => token.kind === kind)
        .map((token) => token.text);
}

test("splits highlighted code into lines that spell the source exactly", () => {
    const code = `const a = 1;
/* spans
lines */ let b = "x";`;

    const lines = highlight("app.ts", code);

    // Diff rows cut tokens by character offset, so the tokens must rebuild each line.
    expect(
        lines.map((tokens) => tokens.map((token) => token.text).join("")),
    ).toEqual(code.split("\n"));
    // Highlighting the block at once keeps the second line of the comment a comment.
    expect(tokensOfKind(lines, "comment")).toEqual(["/* spans", "lines */"]);
    expect(tokensOfKind(lines, "keyword")).toEqual(["const", "let"]);
});

test("highlights legacy stream modes such as shell and TOML", () => {
    const shell = highlight("deploy.sh", `echo "hi" # say hi`);
    const toml = highlight("Cargo.toml", `name = "redoor" # crate`);

    // StreamLanguage grammars parse outside an editor, so their files keep colours too.
    expect(tokensOfKind(shell, "string")).toEqual(['"hi"']);
    expect(tokensOfKind(shell, "comment")).toEqual(["# say hi"]);
    expect(tokensOfKind(toml, "string")).toEqual(['"redoor"']);
    expect(tokensOfKind(toml, "comment")).toEqual(["# crate"]);
});

test("detects an extensionless script by its shebang", () => {
    const lines = highlight(
        "deploy",
        `#!/usr/bin/env python3
def main(): pass`,
    );

    // Scripts without an extension highlight like the editor shows them.
    expect(tokensOfKind(lines, "keyword")).toContain("def");
});

test("resolves fence labels to bundled or lazily loaded grammars", () => {
    // Short labels and full names both reach the grammar the editor bundles.
    expect(languageForName("ts")).toBeInstanceOf(Language);
    expect(languageForName("bash")).toBeInstanceOf(Language);
    expect(languageForName("Python")).toBeInstanceOf(Language);
    // Other known languages are described for loading rather than bundled.
    expect(languageForName("sql")).not.toBeInstanceOf(Language);
    expect(languageForName("sql")).not.toBeNull();
    // Unknown labels get no grammar, so the fence stays plain.
    expect(languageForName("unknown-language")).toBeNull();
});
