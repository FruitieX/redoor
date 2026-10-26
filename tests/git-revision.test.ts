import { mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { $ } from "zx";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { Agent } from "#ui/api-client";
import type { GitCommitDiffResponse } from "#bindings/GitCommitDiffResponse";
import type { GitRevisionTreeResponse } from "#bindings/GitRevisionTreeResponse";
import {
    ProcessManager,
    TempFileManager,
    startServerAndAgent,
} from "./test-utils";

const AGENT_NAME = "git-revision-rest-test-agent";

describe("Git revision REST API", () => {
    const processManager = new ProcessManager();
    const tempFiles = new TempFileManager();
    let serverPort: number;
    let testAgent: Agent;
    let repo: string;
    let firstCommit: string;
    let secondCommit: string;

    beforeAll(async () => {
        const setup = await startServerAndAgent({
            processManager,
            agentName: AGENT_NAME,
            agentCwd: tempFiles.tempDirectory({ suffix: "-agent-cwd" }),
        });

        serverPort = setup.serverPort;
        testAgent = setup.testAgent;

        repo = tempFiles.tempDirectory({ suffix: "-git-revision" });
        await $({ cwd: repo })`git init -b main`;
        await $({
            cwd: repo,
        })`git config user.email redoor-tests@example.invalid`;
        await $({ cwd: repo })`git config user.name Redoor Tests`;

        await mkdir(join(repo, "docs"));
        await writeFile(join(repo, "readme.txt"), "first version\n");
        await writeFile(join(repo, "docs", "guide.md"), "# Guide\n");
        await writeFile(join(repo, "docs", "removed.txt"), "soon gone\n");
        await writeFile(join(repo, "binary.dat"), Buffer.from([0, 159, 1]));
        await $({ cwd: repo })`git add .`;
        await $({ cwd: repo })`git commit -m ${"First commit"}`;
        await $({ cwd: repo })`git tag v1`;
        firstCommit = (
            await $({ cwd: repo })`git rev-parse HEAD`
        ).stdout.trim();

        await writeFile(join(repo, "readme.txt"), "second version\n");
        await rm(join(repo, "docs", "removed.txt"));
        await writeFile(join(repo, "docs", "added.txt"), "new file\n");
        await $({ cwd: repo })`git add -A`;
        await $({ cwd: repo })`git commit -m ${"Second commit"}`;
        secondCommit = (
            await $({ cwd: repo })`git rev-parse HEAD`
        ).stdout.trim();

        // Uncommitted edits must never leak into revision views.
        await writeFile(join(repo, "readme.txt"), "worktree version\n");
    }, 30_000);

    afterAll(async () => {
        await processManager.killAll();
        tempFiles.cleanup();
    });

    /** Fetches a Git route with a revision, returning the status so failures can be asserted. */
    async function revisionRequest(
        route: "tree" | "blob" | "commit-diff",
        path: string,
        rev: string,
    ): Promise<Response> {
        const encodedPath = path
            .split("/")
            .map((part) => encodeURIComponent(part))
            .join("/");

        return fetch(
            `http://127.0.0.1:${serverPort}/api/v1/agents/${encodeURIComponent(testAgent.id)}/git/${route}${encodedPath}?${new URLSearchParams({ rev }).toString()}`,
            { headers: testAgent.getAuthHeaders() },
        );
    }

    it("lists a directory as committed at a tag, branch and commit id", async () => {
        const atTag = await testAgent.gitRevisionTree(join(repo, "docs"), "v1");

        // The tag peels to the first commit, so its summary identifies the revision.
        expect(atTag.commit.id).toBe(firstCommit);
        expect(atTag.commit.summary).toBe("First commit");
        // The requested text is echoed back so the UI can keep showing a branch or tag name.
        expect(atTag.rev).toBe("v1");
        // The deleted file is still listed in the old tree, and the later one is not.
        expect(atTag.entry).toEqual({
            type: "directory",
            entries: [
                { name: "guide.md", kind: "file", size: 8 },
                { name: "removed.txt", kind: "file", size: 10 },
            ],
            truncated: false,
            omitted_non_utf8_entries: 0,
        });

        const atBranch = await testAgent.gitRevisionTree(repo, "main");

        // A branch resolves to its tip.
        expect(atBranch.commit.id).toBe(secondCommit);
        // The root commit's child names its parent, which the banner and diff link to.
        expect(atBranch.commit.parent_ids).toEqual([firstCommit]);
        // The repository root is listed from the commit's own tree.
        expect(atBranch.repository_relative_path).toBe("");

        const atId = await testAgent.gitRevisionTree(
            join(repo, "docs"),
            secondCommit.slice(0, 10),
        );

        // An abbreviated commit id resolves like the full one.
        expect(atId.commit.id).toBe(secondCommit);
        // The newer tree shows the addition and not the deletion.
        expect(
            atId.entry.type === "directory"
                ? atId.entry.entries.map((entry) => entry.name)
                : [],
        ).toEqual(["added.txt", "guide.md"]);
    });

    it("describes files and returns committed text, not the worktree", async () => {
        const readme = await testAgent.gitRevisionTree(
            join(repo, "readme.txt"),
            firstCommit,
        );

        // Small UTF-8 blobs are editable, which opens the read-only editor.
        expect(readme.entry).toMatchObject({
            type: "file",
            size: 14,
            executable: false,
            editable: true,
        });

        const content = await testAgent.gitRevisionContent(
            join(repo, "readme.txt"),
            firstCommit,
        );

        // The content comes from the commit even though the worktree file was changed since.
        expect(content.content).toBe("first version\n");

        const binary = await testAgent.gitRevisionTree(
            join(repo, "binary.dat"),
            "v1",
        );

        // Invalid UTF-8 keeps a binary file out of the editor.
        expect(binary.entry).toMatchObject({ type: "file", editable: false });

        const binaryContent = await revisionRequest(
            "blob",
            join(repo, "binary.dat"),
            "v1",
        );

        // Asking for binary text fails as bad input instead of returning mangled bytes.
        expect(binaryContent.status).toBe(400);
    });

    it("reaches paths deleted from the worktree and rejects missing ones", async () => {
        const removed = await revisionRequest(
            "tree",
            join(repo, "docs", "removed.txt"),
            "v1",
        );

        // A file deleted since the revision is still browsable at that revision.
        expect(removed.status).toBe(200);

        const missing = await revisionRequest(
            "tree",
            join(repo, "docs", "added.txt"),
            "v1",
        );

        // A path the revision lacks is reported as missing, like the worktree browser.
        expect(missing.status).toBe(404);

        const unknown = await revisionRequest("tree", repo, "no-such-branch");

        // An unknown revision is bad input rather than a missing path.
        expect(unknown.status).toBe(400);

        const control = await revisionRequest("tree", repo, "main\nfake");

        // Control characters are rejected before any agent work.
        expect(control.status).toBe(400);

        const outside = await revisionRequest(
            "tree",
            tempFiles.tempDirectory({ suffix: "-not-a-repository" }),
            "main",
        );

        // Paths outside a repository have no revisions.
        expect(outside.status).toBe(400);
    });

    it("diffs a commit against its parent, scoped to the browsed path", async () => {
        const whole: GitCommitDiffResponse = await testAgent.gitCommitDiff(
            repo,
            secondCommit,
        );

        // The commit is compared with its only parent.
        expect(whole.parent_id).toBe(firstCommit);
        // Every changed file is listed in path order with its change kind.
        expect(
            whole.files.map((file) => [
                file.repository_relative_path,
                file.change,
            ]),
        ).toEqual([
            ["docs/added.txt", "added"],
            ["docs/removed.txt", "deleted"],
            ["readme.txt", "modified"],
        ]);

        const readme = whole.files.find(
            (file) => file.repository_relative_path === "readme.txt",
        );

        // Files carry absolute worktree paths so the browser can link to them.
        expect(readme?.path).toBe(join(repo, "readme.txt"));
        // The patch replaces the committed line, not the uncommitted worktree text.
        expect(readme?.result).toMatchObject({ type: "text" });
        expect(
            readme?.result.type === "text" ? readme.result.unified_diff : "",
        ).toContain("-first version\n+second version");

        const docs = await testAgent.gitCommitDiff(
            join(repo, "docs"),
            secondCommit,
        );

        // A directory scopes the diff to its own subtree.
        expect(docs.files.map((file) => file.repository_relative_path)).toEqual(
            ["docs/added.txt", "docs/removed.txt"],
        );

        const single = await testAgent.gitCommitDiff(
            join(repo, "readme.txt"),
            secondCommit,
        );

        // A file path diffs only that file.
        expect(
            single.files.map((file) => file.repository_relative_path),
        ).toEqual(["readme.txt"]);

        const root: GitCommitDiffResponse = await testAgent.gitCommitDiff(
            repo,
            firstCommit,
        );

        // A root commit has no parent and adds every file.
        expect(root.parent_id).toBeNull();
        expect(root.files.every((file) => file.change === "added")).toBe(true);

        const binary = root.files.find(
            (file) => file.repository_relative_path === "binary.dat",
        );

        // Binary content has an explicit outcome instead of a garbled patch.
        expect(binary?.result).toEqual({ type: "binary" });
    });

    it("keeps the revision route responses typed for the browser", async () => {
        const response = await revisionRequest("tree", repo, "v1");
        const body: GitRevisionTreeResponse = await response.json();

        // The raw REST shape matches the generated binding the UI consumes.
        expect(body.entry.type).toBe("directory");
        expect(body.repository_root).toBe(repo);
    });
});
