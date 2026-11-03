use super::*;

/// A bare "remote" with one commit on `main`, as a local path URL.
fn remote() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().unwrap();
    let bare = root.path().join("origin.git");
    let seed = root.path().join("seed");
    let git = |dir: &Path, args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(
        root.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(root.path(), &["init", "-q", "-b", "main", "seed"]);
    std::fs::write(seed.join("README.md"), "hello\n").unwrap();
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "-q", "origin", "main"]);
    (root, bare.to_string_lossy().into_owned())
}

fn options(root: &Path, url: &str, name: &str) -> RepoOptions {
    RepoOptions {
        url: url.into(),
        base: None,
        branch: None,
        dir: root.join(name),
        branch_prefix: "gitagent/".into(),
        author: "gitagent <gitagent@localhost>".into(),
        token: Some("supersecret-token-123".into()),
        helper_dir: root.join(format!("{name}-helper")),
    }
}

fn remote_branches(url: &str) -> String {
    let out = Command::new("git")
        .args(["ls-remote", "--heads", url])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn clones_branches_commits_and_pushes() {
    let (root, url) = remote();
    let repo =
        Repo::clone_for_task(&options(root.path(), &url, "work"), "Fix the failing test!").unwrap();
    assert_eq!(repo.base, "main");
    assert!(
        repo.branch.starts_with("gitagent/fix-the-failing-test-"),
        "{}",
        repo.branch
    );
    assert!(!repo.resumed);

    // Nothing changed: no commit.
    assert_eq!(repo.commit("gitagent: nothing").unwrap(), None);

    std::fs::write(repo.dir.join("fix.txt"), "fixed\n").unwrap();
    let sha = repo.commit("gitagent: fix").unwrap().expect("a commit");
    assert_eq!(sha.len(), 40);
    let log = Command::new("git")
        .current_dir(&repo.dir)
        .args(["log", "-1", "--format=%an <%ae>|%s"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "gitagent <gitagent@localhost>|gitagent: fix"
    );

    repo.push().unwrap();
    let heads = remote_branches(&url);
    assert!(
        heads.contains(&format!("refs/heads/{}", repo.branch)),
        "{heads}"
    );
    // main is untouched.
    let main_log = Command::new("git")
        .args(["--git-dir", &url, "log", "--format=%s", "main"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&main_log.stdout).trim(), "init");
}

#[test]
fn resumes_an_existing_branch() {
    let (root, url) = remote();
    let mut first = options(root.path(), &url, "first");
    first.branch = Some("feature/x".into());
    let repo = Repo::clone_for_task(&first, "t").unwrap();
    assert!(!repo.resumed);
    std::fs::write(repo.dir.join("a.txt"), "a\n").unwrap();
    repo.commit("gitagent: a").unwrap();
    repo.push().unwrap();

    let mut second = options(root.path(), &url, "second");
    second.branch = Some("feature/x".into());
    let again = Repo::clone_for_task(&second, "t").unwrap();
    assert!(again.resumed);
    assert_eq!(
        std::fs::read_to_string(again.dir.join("a.txt")).unwrap(),
        "a\n"
    );
}

#[test]
fn refuses_the_base_branch_and_never_forces() {
    let (root, url) = remote();
    let mut o = options(root.path(), &url, "base");
    o.branch = Some("main".into());
    let err = Repo::clone_for_task(&o, "t").unwrap_err().to_string();
    assert!(err.contains("refusing to work on the base branch"), "{err}");

    // Two clones push different histories to the same branch: the second
    // push is rejected rather than forced.
    let mut a = options(root.path(), &url, "a");
    a.branch = Some("shared".into());
    let ra = Repo::clone_for_task(&a, "t").unwrap();
    let mut b = options(root.path(), &url, "b");
    b.branch = Some("shared".into());
    let rb = Repo::clone_for_task(&b, "t").unwrap();
    std::fs::write(ra.dir.join("a.txt"), "a\n").unwrap();
    ra.commit("a").unwrap();
    ra.push().unwrap();
    std::fs::write(rb.dir.join("b.txt"), "b\n").unwrap();
    rb.commit("b").unwrap();
    assert!(rb.push().is_err());

    // Committing after the agent switched to the base branch is refused.
    Command::new("git")
        .current_dir(&ra.dir)
        .args(["checkout", "-q", "main"])
        .output()
        .unwrap();
    std::fs::write(ra.dir.join("c.txt"), "c\n").unwrap();
    assert!(ra
        .commit("c")
        .unwrap_err()
        .to_string()
        .contains("base branch"));
}

#[test]
fn the_token_never_lands_in_the_repository() {
    let (root, url) = remote();
    let repo = Repo::clone_for_task(&options(root.path(), &url, "tok"), "t").unwrap();
    std::fs::write(repo.dir.join("x"), "x\n").unwrap();
    repo.commit("x").unwrap();
    repo.push().unwrap();
    let mut stack = vec![repo.dir.join(".git")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(bytes) = std::fs::read(&p) {
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("supersecret"),
                    "token found in {}",
                    p.display()
                );
            }
        }
    }
    let helper = std::fs::read_to_string(root.path().join("tok-helper/git-askpass.sh")).unwrap();
    assert!(!helper.contains("supersecret"));
}

#[test]
fn an_earlier_clone_is_reused_and_its_branch_resumed() {
    let (root, url) = remote();
    let mut opts = options(root.path(), &url, "work");
    opts.branch = Some("gitagent/session-1".into());
    let first = Repo::clone_for_task(&opts, "first").unwrap();
    std::fs::write(first.dir.join("a.txt"), "a\n").unwrap();
    // Run records are not part of the work.
    std::fs::create_dir_all(first.dir.join(".gitagent/sessions/s1")).unwrap();
    std::fs::write(first.dir.join(".gitagent/audit.jsonl"), "{}\n").unwrap();
    std::fs::write(first.dir.join(".gitagent/state.json"), "{}\n").unwrap();
    std::fs::write(
        first.dir.join(".gitagent/sessions/s1/messages.json"),
        "[]\n",
    )
    .unwrap();
    first.commit("first").unwrap();
    let shown = Command::new("git")
        .current_dir(&first.dir)
        .args(["show", "--name-only", "--format=", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&shown.stdout).trim(), "a.txt");
    assert!(first.commit("again").unwrap().is_none());
    // Not pushed: the second session keeps the local commit.
    let again = Repo::clone_for_task(&opts, "second").unwrap();
    assert!(again.resumed);
    assert_eq!(again.branch, "gitagent/session-1");
    assert!(again.dir.join("a.txt").is_file());
    // A new session in the same clone branches from the base.
    opts.branch = None;
    let fresh = Repo::clone_for_task(&opts, "third task").unwrap();
    assert!(fresh.branch.starts_with("gitagent/third-task-"));
    assert!(!fresh.dir.join("a.txt").exists());
    // Uncommitted work is never thrown away.
    std::fs::write(fresh.dir.join("b.txt"), "b\n").unwrap();
    let err = Repo::clone_for_task(&opts, "fourth").unwrap_err();
    assert!(err.to_string().contains("uncommitted changes"), "{err}");
    std::fs::remove_file(fresh.dir.join("b.txt")).unwrap();
    // A clone of something else is refused.
    let (_other_root, other) = remote();
    let mut elsewhere = options(root.path(), &other, "work");
    elsewhere.branch = None;
    let err = Repo::clone_for_task(&elsewhere, "x").unwrap_err();
    assert!(err.to_string().contains("is a clone of"), "{err}");
}

#[test]
fn slugs_and_non_empty_targets() {
    assert_eq!(slug("Fix the failing test!"), "fix-the-failing-test");
    assert_eq!(slug("   ***   "), "task");
    assert!(slug(&"a".repeat(100)).len() <= 41);
    let (root, url) = remote();
    std::fs::create_dir_all(root.path().join("busy")).unwrap();
    std::fs::write(root.path().join("busy/file"), "x").unwrap();
    let err = Repo::clone_for_task(&options(root.path(), &url, "busy"), "t").unwrap_err();
    assert!(err.to_string().contains("not empty"));
}

#[test]
fn askpass_helper_answers_user_and_password_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_askpass(&dir.path().join("h")).unwrap();
    let ask = |prompt: &str| {
        let out = Command::new("sh")
            .arg(&helper)
            .arg(prompt)
            .env(TOKEN_VAR, "tok-value")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(ask("Username for 'https://github.com': "), "x-access-token");
    assert_eq!(
        ask("Password for 'https://x-access-token@github.com': "),
        "tok-value"
    );
}
