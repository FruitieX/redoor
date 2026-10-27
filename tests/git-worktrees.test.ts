import { mkdir, rm, utimes, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { $ } from "zx";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { Agent } from "#ui/api-client";
import type { GitWorktreeListResponse } from "#bindings/GitWorktreeListResponse";
import type { GitWorktreeStatusResponse } from "#bindings/GitWorktreeStatusResponse";
import {
    ProcessManager,
    TempFileManager,
    startServerAndAgent,
} from "./test-utils";

const AGENT_NAME = "git-worktrees-rest-test-agent";

describe("Git worktree list and status REST API", () => {
    const processManager = new ProcessManager();
    const tempFiles = new TempFileManager();
    let serverPort: number;
    let testAgent: Agent;
    let repo: string;
    let worktrees: Record<"feature" | "detached" | "missing", string>;
    let commits: Record<"base" | "feature", string>;

    /** Runs git in `cwd` with fixed dates, so activity ordering is deterministic. */
    function git(cwd: string, seconds: number) {
        const date = `@${seconds} +0000`;

        return $({
            cwd,
            env: {
                ...process.env,
                GIT_AUTHOR_DATE: date,
                GIT_COMMITTER_DATE: date,
            },
        });
    }

    /** Reads the commit id HEAD names in a checkout. */
    async function head(cwd: string): Promise<string> {
        return (await $({ cwd })`git rev-parse HEAD`).stdout.trim();
    }

    /** Fetches a Git route raw, so rejected paths and the JSON shape can be asserted. */
    async function gitRequest(
        route: "worktrees" | "worktree-status",
        path: string,
    ): Promise<Response> {
        const encodedPath = path
            .split("/")
            .map((part) => encodeURIComponent(part))
            .join("/");

        return fetch(
            `http://127.0.0.1:${serverPort}/api/v1/agents/${encodeURIComponent(testAgent.id)}/git/${route}${encodedPath}`,
            { headers: testAgent.getAuthHeaders() },
        );
    }

    /** Sets when a checkout's index was last written, which the list reads as activity. */
    async function setIndexTime(gitDir: string, seconds: number) {
        await utimes(join(gitDir, "index"), seconds, seconds);
    }

    beforeAll(async () => {
        const setup = await startServerAndAgent({
            processManager,
            agentName: AGENT_NAME,
            agentCwd: tempFiles.tempDirectory({ suffix: "-agent-cwd" }),
        });

        serverPort = setup.serverPort;
        testAgent = setup.testAgent;

        repo = tempFiles.tempDirectory({ suffix: "-git-worktrees" });
        await $({ cwd: repo })`git init -b main`;
        await $({
            cwd: repo,
        })`git config user.email redoor-tests@example.invalid`;
        await $({ cwd: repo })`git config user.name Redoor Tests`;
        await writeFile(join(repo, "readme.txt"), "first\n");
        await git(repo, 1000)`git add .`;
        await git(repo, 1000)`git commit -m ${"Initial commit"}`;

        const parent = tempFiles.tempDirectory({ suffix: "-linked" });
        worktrees = {
            feature: join(parent, "feature"),
            detached: join(parent, "detached"),
            missing: join(parent, "missing"),
        };

        // A branch worktree with a newer commit than its index write, so its activity is
        // its commit time, and an uncommitted edit.
        await git(repo, 1000)`git worktree add -b feature ${worktrees.feature}`;
        await writeFile(join(worktrees.feature, "readme.txt"), "feature\n");
        await git(worktrees.feature, 3000)`git commit -am ${"Feature work"}`;
        await writeFile(join(worktrees.feature, "readme.txt"), "edited\n");
        await $({ cwd: repo })`git worktree lock ${worktrees.feature}`;

        // A detached worktree on the old commit whose index write is the newest activity,
        // with an untracked file.
        await git(
            repo,
            1000,
        )`git worktree add --detach ${worktrees.detached} main`;
        await writeFile(join(worktrees.detached, "notes.txt"), "notes\n");

        // A registered worktree whose directory was deleted outside Git.
        await git(repo, 1000)`git worktree add -b gone ${worktrees.missing}`;
        await rm(worktrees.missing, { recursive: true });

        // Git wrote every index just now, so pin their times after the last Git command to
        // make the activity order deterministic.
        await setIndexTime(join(repo, ".git"), 1000);
        await setIndexTime(join(repo, ".git", "worktrees", "feature"), 2000);
        await setIndexTime(join(repo, ".git", "worktrees", "detached"), 5000);

        commits = {
            base: await head(repo),
            feature: await head(worktrees.feature),
        };
    }, 30_000);

    afterAll(async () => {
        await processManager.killAll();
        tempFiles.cleanup();
    });

    it("lists the main worktree first and then the most recently active", async () => {
        const list = await testAgent.gitWorktrees(repo);

        // Main leads despite the oldest activity, the newest index write beats a newer
        // commit, and the missing checkout trails because nobody can work in it.
        expect(list.worktrees.map((worktree) => worktree.path)).toEqual([
            repo,
            worktrees.detached,
            worktrees.feature,
            worktrees.missing,
        ]);
        expect(list.repository_root).toBe(repo);
        expect(list.truncated).toBe(false);

        const [main, detached, feature, missing] = list.worktrees;

        // The main checkout is marked as the one the request came from.
        expect(main).toMatchObject({
            is_main: true,
            is_current: true,
            branch_name: "main",
            last_activity_at: 1000,
            locked: false,
            missing: false,
        });
        expect(main?.head_commit?.id).toBe(commits.base);

        // A detached HEAD has no branch, and its index write is newer than its commit.
        expect(detached).toMatchObject({
            is_main: false,
            is_current: false,
            branch_name: null,
            last_activity_at: 5000,
        });
        expect(detached?.head_commit?.id).toBe(commits.base);

        // The lock is reported, and the newer commit time wins over the older index write.
        expect(feature).toMatchObject({
            branch_name: "feature",
            last_activity_at: 3000,
            locked: true,
            missing: false,
        });
        expect(feature?.head_commit?.summary).toBe("Feature work");

        // A deleted checkout keeps its branch from the registration.
        expect(missing).toMatchObject({
            branch_name: "gone",
            missing: true,
        });
    });

    it("counts each checkout's tracked and untracked changes", async () => {
        // The main checkout has no uncommitted work.
        expect(await testAgent.gitWorktreeStatus(repo)).toEqual({
            changed: 0,
            untracked: 0,
            truncated: false,
        });
        // New files count as untracked, not changed.
        expect(await testAgent.gitWorktreeStatus(worktrees.detached)).toEqual({
            changed: 0,
            untracked: 1,
            truncated: false,
        });
        // A tracked edit counts as changed.
        expect(await testAgent.gitWorktreeStatus(worktrees.feature)).toEqual({
            changed: 1,
            untracked: 0,
            truncated: false,
        });
    });

    it("answers every status of a long list that the agent queues", async () => {
        const paths = Array.from({ length: 4 }, () => [
            repo,
            worktrees.detached,
            worktrees.feature,
        ]).flat();

        const statuses = await Promise.all(
            paths.map((path) => testAgent.gitWorktreeStatus(path)),
        );

        // More requests than the agent walks at once all finish, each with its own counts.
        expect(statuses.map((status) => status.untracked)).toEqual(
            paths.map((path) => (path === worktrees.detached ? 1 : 0)),
        );
        expect(statuses.map((status) => status.changed)).toEqual(
            paths.map((path) => (path === worktrees.feature ? 1 : 0)),
        );
    });

    it("lists the same worktrees from inside a linked worktree", async () => {
        const nested = join(worktrees.feature, "src");
        await mkdir(nested, { recursive: true });

        const list = await testAgent.gitWorktrees(nested);

        // The order does not depend on which checkout the request came from.
        expect(list.worktrees.map((worktree) => worktree.path)).toEqual([
            repo,
            worktrees.detached,
            worktrees.feature,
            worktrees.missing,
        ]);
        // Only the checkout containing the requested path is current.
        expect(
            list.worktrees
                .filter((worktree) => worktree.is_current)
                .map((worktree) => worktree.path),
        ).toEqual([worktrees.feature]);
        // The response names the checkout the request resolved, like the other Git lists.
        expect(list.repository_root).toBe(worktrees.feature);
    });

    it("returns the generated JSON shapes and rejects paths outside a repository", async () => {
        const response = await gitRequest("worktrees", repo);

        // The route answers plain GETs at a filesystem path.
        expect(response.status).toBe(200);

        const list: GitWorktreeListResponse = await response.json();

        // The raw REST shape matches the generated binding the UI consumes.
        expect(list.worktrees[0]?.is_main).toBe(true);

        const outside = tempFiles.tempDirectory({
            suffix: "-git-worktrees-outside",
        });

        // A path outside a repository has no worktrees to list.
        expect((await gitRequest("worktrees", outside)).status).toBe(400);

        const nested = join(worktrees.feature, "src");
        await mkdir(nested, { recursive: true });
        const statusResponse = await gitRequest("worktree-status", nested);

        // A path inside a checkout counts the whole checkout, not just that directory.
        expect(statusResponse.status).toBe(200);
        const status: GitWorktreeStatusResponse = await statusResponse.json();
        expect(status).toEqual({ changed: 1, untracked: 0, truncated: false });

        // A path outside a repository has no checkout to count.
        expect((await gitRequest("worktree-status", outside)).status).toBe(400);
    });
});
