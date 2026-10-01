import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

function runGit(args: string[], cwd: string) {
    execFileSync("git", args, {
        cwd,
        stdio: "pipe",
    });
}

function createRepo(): string {
    const repo = mkdtempSync(join(tmpdir(), "gitagent-repo-"));

    runGit(["init", "-b", "main"], repo);
    runGit(["config", "user.email", "test@example.com"], repo);
    runGit(["config", "user.name", "GitAgent Test"], repo);

    writeFileSync(join(repo, "README.md"), "test\n");

    runGit(["add", "."], repo);
    runGit(["commit", "-m", "initial"], repo);

    return repo;
}

describe("initLocalSession repo safety", () => {
    it("rejects an existing directory whose origin does not match the requested repo", async () => {
        const dir = mkdtempSync(join(tmpdir(), "gitagent-session-"));
        const existingRepo = createRepo();
        const requestedRepo = createRepo();

        try {
            // Make dir a valid Git repository.
            runGit(["init", "-b", "main"], dir);
            runGit(["config", "user.email", "test@example.com"], dir);
            runGit(["config", "user.name", "GitAgent Test"], dir);

            writeFileSync(join(dir, "README.md"), "existing\n");

            runGit(["add", "."], dir);
            runGit(["commit", "-m", "initial"], dir);

            // This repository belongs to existingRepo, not requestedRepo.
            runGit(["remote", "add", "origin", existingRepo], dir);

            const { initLocalSession } = await import("../dist/session.js");

            assert.throws(
                () =>
                    initLocalSession({
                        url: requestedRepo,
                        token: "test-token",
                        dir,
                    }),
                /origin.*match|different.*repository|repository.*mismatch/i,
            );
        } finally {
            rmSync(dir, { recursive: true, force: true });
            rmSync(existingRepo, { recursive: true, force: true });
            rmSync(requestedRepo, { recursive: true, force: true });
        }
    });
});