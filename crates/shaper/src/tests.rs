use super::*;

fn cfg() -> ShapeCfg {
    ShapeCfg {
        cap_tokens: 8000,
        head_ratio: 0.3,
        collapse_min_run: 3,
    }
}

fn run(raw: &[u8], cfg: &ShapeCfg) -> (tempfile::TempDir, Shaped) {
    let dir = tempfile::tempdir().unwrap();
    let shaped = shape(raw, cfg, &dir.path().join("out"), 7);
    (dir, shaped)
}

#[test]
fn short_output_passes_through_and_is_spilled() {
    let (_d, s) = run(b"hello\nworld\n", &cfg());
    assert_eq!(s.text, "hello\nworld\n");
    assert_eq!((s.total_lines, s.hidden_lines, s.collapsed_runs), (2, 0, 0));
    let path = s.spill_path.unwrap();
    assert!(path.ends_with("out/7.log"));
    assert_eq!(std::fs::read(path).unwrap(), b"hello\nworld\n");
}

#[test]
fn strips_ansi_sequences_and_backspaces() {
    let raw = "\x1b[1;32mPASS\x1b[0m test \x1b]0;title\x07done\x1b(B\nab\x08c\n";
    let (_d, s) = run(raw.as_bytes(), &cfg());
    assert_eq!(s.text, "PASS test done\nac\n");
}

#[test]
fn progress_bars_keep_their_last_frame_and_crlf_is_kept_intact() {
    let raw = b"Downloading  10%\rDownloading  55%\rDownloading 100%\nline with crlf\r\n";
    let (_d, s) = run(raw, &cfg());
    assert_eq!(s.text, "Downloading 100%\nline with crlf\n");
}

#[test]
fn collapses_runs_of_similar_lines() {
    let mut raw = String::from("start\n");
    for i in 0..50 {
        raw.push_str(&format!("Compiling crate-{i} v0.{i}.0\n"));
    }
    raw.push_str("Finished\n");
    let (_d, s) = run(raw.as_bytes(), &cfg());
    assert_eq!(
        s.text,
        "start\nCompiling crate-0 v0.0.0\n[48 similar lines hidden]\nCompiling crate-49 v0.49.0\nFinished\n"
    );
    assert_eq!(
        (s.hidden_lines, s.collapsed_runs, s.total_lines),
        (48, 1, 52)
    );
}

#[test]
fn short_runs_and_disabled_collapsing_are_left_alone() {
    let (_d, s) = run(b"a1\na2\nb\n", &cfg());
    assert_eq!(s.text, "a1\na2\nb\n");
    let mut off = cfg();
    off.collapse_min_run = 0;
    let raw: String = (0..10).map(|i| format!("x{i}\n")).collect();
    let (_d, s) = run(raw.as_bytes(), &off);
    assert_eq!(s.text, raw);
}

#[test]
fn caps_long_output_keeping_head_and_tail() {
    let mut c = cfg();
    c.cap_tokens = 200;
    c.collapse_min_run = 0;
    let raw: String = (0..5000).map(|i| format!("line {i} of output\n")).collect();
    let (_d, s) = run(raw.as_bytes(), &c);
    assert!(s.text.starts_with("line 0 of output\n"));
    assert!(s.text.ends_with("line 4999 of output\n"));
    assert!(
        s.text.contains("lines hidden — full output: "),
        "{}",
        s.text
    );
    assert!(estimate_tokens(&s.text) < 300);
    assert!(s.hidden_lines > 4800);
    // About 30% of the kept lines come from the head.
    let shown: Vec<&str> = s.text.lines().collect();
    let marker = shown
        .iter()
        .position(|l| l.contains("lines hidden"))
        .unwrap();
    let tail = shown.len() - marker - 1;
    assert!(marker * 2 < tail, "head {marker} tail {tail}");
}

#[test]
fn caps_a_single_huge_line() {
    let mut c = cfg();
    c.cap_tokens = 100;
    let raw = format!("{}\n", "é".repeat(10_000));
    let (_d, s) = run(raw.as_bytes(), &c);
    assert!(s.text.contains("characters hidden"));
    assert!(s.text.len() < 600);
}

#[test]
fn binary_output_is_described_not_shown() {
    let raw: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let (_d, s) = run(&raw, &cfg());
    assert!(s.binary);
    assert!(s
        .text
        .starts_with("[binary output, 4096 bytes — full output: "));
    assert_eq!(std::fs::read(s.spill_path.unwrap()).unwrap(), raw);
    // Mostly-text output with a few odd bytes is still text.
    let (_d, s) = run("ok café\n".as_bytes(), &cfg());
    assert!(!s.binary);
}

#[test]
fn empty_output_stays_empty() {
    let (_d, s) = run(b"", &cfg());
    assert_eq!(s.text, "");
    assert_eq!(s.total_lines, 0);
}

#[test]
fn shaper_numbers_spill_files_and_spots_spill_reads() {
    let dir = tempfile::tempdir().unwrap();
    let spill_dir = dir.path().join("out");
    let mut shaper = Shaper::new(cfg(), &spill_dir);
    let a = shaper.shape(b"one\n");
    let b = shaper.shape(b"two\n");
    assert!(a.spill_path.unwrap().ends_with("out/1.log"));
    assert!(b.spill_path.unwrap().ends_with("out/2.log"));
    let cmd = format!("grep error {}/2.log", spill_dir.display());
    assert!(shaper.mentions_spill(&cmd));
    assert!(!shaper.mentions_spill("grep error build.log"));
}
