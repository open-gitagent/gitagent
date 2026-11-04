use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    session: Session,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ShellConfig::new(dir.path(), dir.path().join("scripts"));
    let session = Session::start(cfg).await.unwrap();
    Fixture { _dir: dir, session }
}

fn text(r: &ExecResult) -> String {
    String::from_utf8_lossy(&r.output).into_owned()
}

#[tokio::test]
async fn runs_a_command_and_reports_the_exit_code() {
    let mut f = start().await;
    let r = f.session.exec("echo hi", 5000).await.unwrap();
    assert_eq!(text(&r), "hi\n");
    assert_eq!(r.exit, Some(0));
    assert!(r.finished);
    let r = f.session.exec("echo oops >&2; false", 5000).await.unwrap();
    assert_eq!(text(&r), "oops\n");
    assert_eq!(r.exit, Some(1));
}

#[tokio::test]
async fn keeps_state_between_commands() {
    let mut f = start().await;
    f.session
        .exec("mkdir -p sub && cd sub && export GA_X=42", 5000)
        .await
        .unwrap();
    let r = f
        .session
        .exec("echo $GA_X; basename $PWD", 5000)
        .await
        .unwrap();
    assert_eq!(text(&r), "42\nsub\n");
    assert!(f.session.cwd().ends_with("sub"));
}

#[tokio::test]
async fn handles_quotes_heredocs_and_stdin_readers() {
    let mut f = start().await;
    let cmd = "cat > f.txt <<'EOF'\nit's \"quoted\" $HOME\nEOF\ncat f.txt";
    let r = f.session.exec(cmd, 5000).await.unwrap();
    assert_eq!(text(&r), "it's \"quoted\" $HOME\n");
    // An unterminated quote is a syntax error, not a hang.
    let r = f.session.exec("echo 'unterminated", 5000).await.unwrap();
    assert!(r.finished);
    assert_ne!(r.exit, Some(0));
    // A command reading stdin must not consume the marker.
    let r = f
        .session
        .exec("read -t 1 x; echo got:$x", 5000)
        .await
        .unwrap();
    assert!(r.finished);
    assert_eq!(text(&r), "got:\n");
}

#[tokio::test]
async fn yields_partial_output_then_interrupts_on_next_exec() {
    let mut f = start().await;
    let r = f
        .session
        .exec("echo start; sleep 30; echo never", 1000)
        .await
        .unwrap();
    assert!(!r.finished);
    assert_eq!(r.exit, None);
    assert_eq!(text(&r).trim(), "start");
    assert!(f.session.is_running());

    let r = f.session.exec("echo next", 5000).await.unwrap();
    assert_eq!(text(&r), "next\n");
    assert!(r.notes[0].contains("interrupted"), "{:?}", r.notes);
    assert!(!f.session.is_running());
}

#[tokio::test]
async fn interrupts_a_shell_builtin_loop() {
    let mut f = start().await;
    let r = f.session.exec("while true; do :; done", 500).await.unwrap();
    assert!(!r.finished);
    let r = f.session.exec("echo after", 5000).await.unwrap();
    assert_eq!(text(&r), "after\n");
}

#[tokio::test]
async fn poll_collects_the_rest_of_a_running_command() {
    let mut f = start().await;
    let r = f.session.exec("sleep 1; echo done", 100).await.unwrap();
    assert!(!r.finished);
    let r = f.session.poll(5000).await.unwrap();
    assert!(r.finished);
    assert_eq!(text(&r), "done\n");
}

#[tokio::test]
async fn restarts_after_the_shell_exits() {
    let mut f = start().await;
    f.session.exec("cd /", 5000).await.unwrap();
    let r = f.session.exec("exit 3", 5000).await.unwrap();
    assert!(r.finished);
    assert_eq!(r.exit, None);
    assert!(r.notes[0].contains("shell exited"), "{:?}", r.notes);

    let r = f.session.exec("pwd", 5000).await.unwrap();
    assert_eq!(text(&r), "/\n");
    assert!(r.notes[0].contains("new shell"), "{:?}", r.notes);
}

#[tokio::test]
async fn removes_secrets_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ShellConfig::new(dir.path(), dir.path().join("s"));
    cfg.remove_env = vec!["HOME".into()];
    cfg.env = vec![("GA_EXTRA".into(), "yes".into())];
    let mut s = Session::start(cfg).await.unwrap();
    let r = s
        .exec("echo ${HOME:-unset} $GA_EXTRA $PAGER", 5000)
        .await
        .unwrap();
    assert_eq!(text(&r), "unset yes cat\n");
}

#[tokio::test]
async fn large_output_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ShellConfig::new(dir.path(), dir.path().join("s"));
    cfg.max_buffer_bytes = 64 * 1024;
    let mut s = Session::start(cfg).await.unwrap();
    let r = s.exec("seq 1 200000", 20_000).await.unwrap();
    assert!(r.finished);
    assert_eq!(r.exit, Some(0));
    assert!(r.omitted_bytes > 1_000_000);
    let t = text(&r);
    assert!(t.starts_with("1\n2\n"));
    assert!(t.ends_with("200000\n"));
}

/// How long a killed process may take to disappear (reaping is asynchronous
/// once its parent, the shell, is gone).
const PROCESS_GONE_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn process_exists(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

pub(crate) async fn wait_until_gone(pid: i32) -> bool {
    let deadline = Instant::now() + PROCESS_GONE_WAIT;
    while process_exists(pid) {
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

pub(crate) fn parse_pid(r: &ExecResult) -> i32 {
    let out = text(r);
    out.trim()
        .parse()
        .unwrap_or_else(|_| panic!("no pid in {out:?}"))
}

#[tokio::test]
async fn a_command_ignoring_ctrl_c_is_stopped_with_its_shell() {
    let mut f = start().await;
    let r = f
        .session
        .exec("sh -c 'trap \"\" INT; echo $$; exec sleep 300'", 1000)
        .await
        .unwrap();
    let pid = parse_pid(&r);
    let r = f.session.exec("echo next", 15_000).await.unwrap();
    assert_eq!(text(&r), "next\n");
    assert!(r.notes[0].contains("did not stop"), "{:?}", r.notes);
    assert!(wait_until_gone(pid).await, "pid {pid} survived");
}

#[tokio::test]
async fn kill_stops_the_running_foreground_command() {
    let mut f = start().await;
    let r = f
        .session
        .exec("sh -c 'echo $$; exec sleep 300'", 1000)
        .await
        .unwrap();
    assert!(!r.finished);
    let pid = parse_pid(&r);
    assert!(process_exists(pid));
    f.session.kill();
    assert!(wait_until_gone(pid).await, "pid {pid} survived kill");
}

#[tokio::test]
async fn dropping_the_session_stops_background_jobs() {
    let mut f = start().await;
    // The job ignores the SIGHUP the shell passes on to its jobs, so only
    // signalling its own process group stops it.
    let r = f
        .session
        .exec("(trap '' HUP; exec sleep 300) & echo $!", 5000)
        .await
        .unwrap();
    assert!(r.finished);
    let pid = parse_pid(&r);
    assert!(process_exists(pid));
    drop(f);
    assert!(wait_until_gone(pid).await, "pid {pid} survived drop");
}
