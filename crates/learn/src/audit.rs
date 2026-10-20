//! Rejects proposed lessons that carry task-specific details: the task name,
//! paths, file names, quoted identifiers or numbers from the instruction,
//! code, or anything too long to be general advice.

use crate::store::MAX_LESSON_CHARS;

/// Why a lesson was rejected, or `None` if it passes.
pub fn check(lesson: &str, task: &str, instruction: &str) -> Option<String> {
    let text = lesson.trim();
    let lower = text.to_lowercase();
    if text.chars().count() > MAX_LESSON_CHARS {
        return Some(format!("longer than {MAX_LESSON_CHARS} characters"));
    }
    if text.split_whitespace().count() < 5 {
        return Some("too short to be advice".into());
    }
    if text.contains("```") || text.contains('\n') {
        return Some("contains a code block".into());
    }
    if let Some(span) = backtick_spans(text).find(|s| s.chars().count() > 40) {
        return Some(format!("long code span `{span}`"));
    }
    if !task.is_empty() {
        let task = task.to_lowercase();
        for form in [task.clone(), task.replace('-', " "), task.replace('-', "_")] {
            if lower.contains(&form) {
                return Some("names the task".into());
            }
        }
    }
    if let Some(run) = digit_runs(text).find(|d| d.len() >= 3) {
        return Some(format!("contains the number {run}"));
    }
    for token in specific_tokens(instruction) {
        if lower.contains(&token.to_lowercase()) {
            return Some(format!("repeats `{token}` from the task"));
        }
    }
    None
}

/// Paths, file names and quoted identifiers in the instruction.
fn specific_tokens(instruction: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in instruction.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            !(c.is_alphanumeric() || c == '/' || c == '_' || c == '.' || c == '-')
        });
        let token = token.trim_end_matches('.');
        let is_path = token.len() > 2 && token.contains('/');
        // A name with an extension; `e.g` and `i.e` are not file names.
        let is_file = token.rsplit_once('.').is_some_and(|(stem, ext)| {
            stem.chars().count() > 1
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && !ext.chars().all(|c| c.is_ascii_digit())
        });
        if is_path || is_file {
            out.push(token.to_string());
        }
    }
    for span in backtick_spans(instruction) {
        if span.chars().count() >= 4 {
            out.push(span.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn backtick_spans(text: &str) -> impl Iterator<Item = &str> {
    text.split('`').skip(1).step_by(2)
}

fn digit_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTRUCTION: &str =
        "Build a server, e.g. a small one, in /app/kv_server.py using the `KVStore` class \
        and listen on port 5328. Write the result to output.json.";

    #[test]
    fn general_advice_passes() {
        for ok in [
            "Start long-running servers with nohup and check they answer before finishing.",
            "Read the installed library's version before writing code against its API.",
            "After installing packages with pip, import them once to confirm they load.",
            "Check tools exist before using them, e.g. with a version flag.",
        ] {
            assert_eq!(check(ok, "grpc-kv-demo", INSTRUCTION), None, "{ok}");
        }
    }

    #[test]
    fn task_details_are_rejected() {
        let cases = [
            ("For grpc-kv-demo, generate the stubs first before starting.", "names the task"),
            ("When asked for a grpc kv demo server, use the stub generator.", "names the task"),
            ("Always bind the server to port 5328 before testing it.", "number"),
            ("Put the server code in /app/kv_server.py and run it there.", "repeats"),
            ("Subclass the KVStore class rather than writing your own one.", "repeats"),
            ("Remember to write the final answer into output.json at the end.", "repeats"),
            ("Use pip.", "too short"),
            ("Run this: ```pip install grpcio``` before anything else is done.", "code block"),
            ("Use `python -m grpc_tools.protoc -I. --python_out=. --grpc_python_out=. x.proto` to build.", "long code span"),
        ];
        for (lesson, why) in cases {
            let got = check(lesson, "grpc-kv-demo", INSTRUCTION);
            assert!(
                got.as_deref().is_some_and(|g| g.contains(why)),
                "{lesson}: {got:?}"
            );
        }
        assert!(check(&"word ".repeat(200), "", "").is_some());
    }
}
