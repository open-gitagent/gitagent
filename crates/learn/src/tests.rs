use std::path::Path;

use gitagent_loader::{load, AgentSpec, LoadOptions};
use gitagent_provider::ScriptedProvider;

use super::*;

pub(crate) fn write_trial(job: &Path, name: &str, task: &str, reward: Option<f64>) {
    let dir = job.join(name);
    std::fs::create_dir_all(dir.join("agent")).unwrap();
    let mut result = serde_json::json!({ "task_name": format!("terminal-bench/{task}") });
    if let Some(r) = reward {
        result["verifier_result"] = serde_json::json!({ "rewards": { "reward": r } });
    }
    std::fs::write(dir.join("result.json"), result.to_string()).unwrap();
    let trajectory = serde_json::json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "s",
        "agent": { "name": "gitagent", "version": "0" },
        "steps": [
            { "step_id": 1, "source": "system", "message": "SYSTEM PROMPT" },
            { "step_id": 2, "source": "user", "message": format!("# Task\n\nStart the {task} server on port 5328 from /app/server.py.") },
            { "step_id": 3, "source": "agent", "message": "Starting it.",
              "tool_calls": [{ "tool_call_id": "c1", "function_name": "exec", "arguments": { "cmd": "python /app/server.py" } }],
              "observation": { "results": [{ "source_call_id": "c1", "content": "x".repeat(5000) }] } }
        ]
    });
    std::fs::write(dir.join("agent/trajectory.json"), trajectory.to_string()).unwrap();
    // Files the learner must never read.
    let never_read = dir.join("verifier");
    std::fs::create_dir_all(&never_read).unwrap();
    std::fs::write(never_read.join("test-stdout.txt"), "SECRET").unwrap();
}

fn spec() -> AgentSpec {
    load(&LoadOptions {
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap()
}

fn reply(json: &str) -> String {
    format!("- text: {}\n", serde_json::to_string(json).unwrap())
}

struct Setup {
    dir: tempfile::TempDir,
    opts: LearnOptions,
}

/// Two jobs: `serve-thing` passed once and failed once, `other-thing`
/// passed, `heldout-thing` is not in the dev split.
fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let (j1, j2) = (dir.path().join("job1"), dir.path().join("job2"));
    write_trial(&j1, "serve-thing__a", "serve-thing", Some(1.0));
    write_trial(&j1, "other-thing__a", "other-thing", Some(1.0));
    write_trial(&j2, "serve-thing__b", "serve-thing", Some(0.0));
    write_trial(&j2, "heldout-thing__a", "heldout-thing", Some(0.0));
    let lessons_path = dir.path().join("prompts/lessons.md");
    std::fs::create_dir_all(lessons_path.parent().unwrap()).unwrap();
    std::fs::write(&lessons_path, "<!--\nheader\n-->\n").unwrap();
    let opts = LearnOptions {
        jobs: vec![j1, j2],
        store_path: dir.path().join("lessons/lessons.json"),
        lessons_path,
        dev_tasks: Some(["serve-thing", "other-thing"].map(String::from).into()),
        max_chars: 3000,
        min_tasks: 2,
        dry_run: false,
    };
    Setup { dir, opts }
}

const NOHUP: &str = "Start long-running servers with nohup and check they answer before finishing.";
const VERSIONS: &str = "Read the installed library's version before writing code against its API.";

#[tokio::test]
async fn learns_audits_and_renders_lessons_from_dev_trials_only() {
    let s = setup();
    let script = reply(&format!(
        r#"{{"ops": [{{"op": "add", "text": "{NOHUP}"}}, {{"op": "add", "text": "Put the code in /app/server.py so it is found."}}]}}"#
    )) + &reply(&format!(
        r#"{{"ops": [{{"op": "upvote", "id": 1}}, {{"op": "add", "text": "{VERSIONS}"}}]}}"#
    ));
    let provider = ScriptedProvider::from_yaml(&script).unwrap();
    let mut report = learn(&spec(), &provider, &s.opts).await.unwrap();
    // VERSIONS comes from one task only, so it is stored but not shown yet.
    assert_eq!(report.rendered, format!("- {NOHUP}\n"));
    report.rendered.clear();
    assert_eq!(
        report,
        LearnReport {
            trials_read: 4,
            trials_not_dev: 1,
            trials_learned: 3,
            model_calls: 2,
            ops_proposed: 4,
            ops_rejected: 1,
            lessons_added: 2,
            lessons_upvoted: 1,
            lessons_total: 2,
            lessons_shown: 1,
            ..Default::default()
        }
    );

    let requests = provider.requests();
    assert!(requests[0].system.starts_with("You keep a short list"));
    assert!(requests[0].tools.is_empty());
    let first = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(first.contains("(none yet)"));
    assert!(first.contains("Attempt A (accepted by the checker)"));
    // The pass/fail pair of one task goes in one request, after the first
    // request's lesson was stored.
    let second = serde_json::to_string(&requests[1].messages).unwrap();
    assert!(second.contains(&format!("[1] {NOHUP}")));
    assert!(second.contains("same task"));
    assert!(second.contains("Attempt B (rejected by the checker)"));
    for r in &requests {
        let text = serde_json::to_string(&r.messages).unwrap();
        assert!(!text.contains("SECRET"), "verifier files are never read");
        assert!(!text.contains("heldout-thing"));
        assert!(!text.contains("SYSTEM PROMPT"));
    }

    let rendered = std::fs::read_to_string(&s.opts.lessons_path).unwrap();
    assert_eq!(rendered, format!("<!--\nheader\n-->\n- {NOHUP}\n"));
    let store = Store::load(&s.opts.store_path).unwrap();
    assert_eq!(store.lessons[0].importance, 3);
    assert_eq!(store.learned.len(), 3);

    // A second pass over the same jobs asks nothing.
    let provider = ScriptedProvider::from_yaml("[]").unwrap();
    let report = learn(&spec(), &provider, &s.opts).await.unwrap();
    assert_eq!((report.trials_already_learned, report.model_calls), (3, 0));
    assert!(provider.requests().is_empty());
    drop(s.dir);
}

#[tokio::test]
async fn a_failed_call_leaves_the_trials_for_next_time() {
    let mut s = setup();
    s.opts.dev_tasks = Some(["other-thing".to_string()].into());
    let provider = ScriptedProvider::from_yaml("- error: fatal\n").unwrap();
    let report = learn(&spec(), &provider, &s.opts).await.unwrap();
    assert_eq!(
        (
            report.model_calls,
            report.model_errors,
            report.trials_learned
        ),
        (1, 1, 0)
    );
    assert!(Store::load(&s.opts.store_path).unwrap().learned.is_empty());
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let mut s = setup();
    s.opts.dry_run = true;
    s.opts.min_tasks = 1;
    s.opts.dev_tasks = Some(["other-thing".to_string()].into());
    let provider = ScriptedProvider::from_yaml(&reply(&format!(
        r#"{{"ops": [{{"op": "add", "text": "{NOHUP}"}}]}}"#
    )))
    .unwrap();
    let report = learn(&spec(), &provider, &s.opts).await.unwrap();
    assert_eq!(report.lessons_added, 1);
    assert_eq!(report.rendered, format!("- {NOHUP}\n"));
    assert!(!s.opts.store_path.exists());
    assert_eq!(
        std::fs::read_to_string(&s.opts.lessons_path).unwrap(),
        "<!--\nheader\n-->\n"
    );
}

#[test]
fn split_files_list_dev_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("split.json");
    std::fs::write(
        &path,
        r#"{"dev": ["terminal-bench/repair-repo", "grpc-kv-demo"], "heldout": ["x"]}"#,
    )
    .unwrap();
    let dev = read_split(&path).unwrap();
    assert_eq!(
        dev,
        ["repair-repo", "grpc-kv-demo"].map(String::from).into()
    );
    std::fs::write(&path, r#"{"heldout": []}"#).unwrap();
    assert!(read_split(&path).is_err());
}

#[test]
fn the_shipped_lessons_file_keeps_its_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lessons.md");
    write_lessons(&path, "- a\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "- a\n");
    std::fs::write(&path, "<!--\nkeep me\n-->\n- old\n").unwrap();
    write_lessons(&path, "- new\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "<!--\nkeep me\n-->\n- new\n"
    );
}

#[tokio::test]
async fn online_learning_writes_into_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("job");
    write_trial(&job, "t__1", "serve-thing", None);
    let trajectory = job.join("t__1/agent/trajectory.json");
    let project = dir.path().join("project");
    let provider = ScriptedProvider::from_yaml(
        &(reply(&format!(
            r#"{{"ops": [{{"op": "add", "text": "{NOHUP}"}}]}}"#
        )) + &reply(r#"{"ops": [{"op": "upvote", "id": 1}]}"#)),
    )
    .unwrap();

    let r = learn_from_run(&spec(), &provider, &trajectory, &project)
        .await
        .unwrap();
    assert_eq!(
        (
            r.trials_read,
            r.trials_learned,
            r.lessons_added,
            r.lessons_shown
        ),
        (1, 1, 1, 0)
    );
    let text = serde_json::to_string(&provider.requests()[0].messages).unwrap();
    assert!(text.contains("not checked by anyone"));
    let lessons = project.join(".gitagent/prompts/lessons.md");
    assert!(std::fs::read_to_string(&lessons)
        .unwrap()
        .ends_with("-->\n"));

    // A second run supports it: each run counts as its own task.
    let r = learn_from_run(&spec(), &provider, &trajectory, &project)
        .await
        .unwrap();
    assert_eq!((r.lessons_upvoted, r.lessons_shown), (1, 1));
    assert!(std::fs::read_to_string(&lessons)
        .unwrap()
        .ends_with(&format!("-->\n- {NOHUP}\n")));
}
