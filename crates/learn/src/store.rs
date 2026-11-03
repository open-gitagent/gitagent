//! The lesson store: general lessons with per-task importance counts.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Importance of a new lesson; it is deleted when it reaches 0.
const NEW_IMPORTANCE: i32 = 2;
/// Longest lesson kept.
pub const MAX_LESSON_CHARS: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lesson {
    pub id: u32,
    pub text: String,
    pub importance: i32,
    pub helpful: u32,
    pub harmful: u32,
    /// Tasks whose attempts added or upvoted it (bookkeeping for the
    /// evidence threshold; never shown to the model).
    #[serde(default)]
    pub tasks: BTreeSet<String>,
    /// Trials it came from or was voted on by (for review; never shown to the model).
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Lesson {
    fn vote_from(&mut self, task: &str, source: &str) {
        if self.tasks.insert(task.to_string()) {
            self.importance += 1;
        }
        self.sources.push(source.to_string());
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Store {
    pub next_id: u32,
    pub lessons: Vec<Lesson>,
    /// Trials already learned from, so re-running on the same jobs is a no-op.
    #[serde(default)]
    pub learned: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Add { text: String },
    Edit { id: u32, text: String },
    Upvote { id: u32 },
    Downvote { id: u32 },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Applied {
    pub added: usize,
    pub edited: usize,
    pub upvoted: usize,
    pub downvoted: usize,
    pub deleted: usize,
    pub duplicates: usize,
    pub unknown_ids: usize,
}

impl Store {
    pub fn load(path: &Path) -> Result<Store> {
        if !path.exists() {
            return Ok(Store::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Applies operations that already passed the audit. `task` is the task
    /// the attempts were for: importance rises only on the first vote from
    /// each task, so repeated runs of one task do not count as wider evidence.
    pub fn apply(&mut self, ops: &[Op], task: &str, source: &str) -> Applied {
        let mut a = Applied::default();
        for op in ops {
            match op {
                Op::Add { text } => {
                    let text = clean(text);
                    if self.lessons.iter().any(|l| similar(&l.text, &text)) {
                        a.duplicates += 1;
                        continue;
                    }
                    self.next_id += 1;
                    self.lessons.push(Lesson {
                        id: self.next_id,
                        text,
                        importance: NEW_IMPORTANCE,
                        helpful: 0,
                        harmful: 0,
                        tasks: BTreeSet::from([task.to_string()]),
                        sources: vec![source.to_string()],
                    });
                    a.added += 1;
                }
                Op::Edit { id, text } => match self.get(*id) {
                    Some(l) => {
                        l.text = clean(text);
                        l.vote_from(task, source);
                        a.edited += 1;
                    }
                    None => a.unknown_ids += 1,
                },
                Op::Upvote { id } => match self.get(*id) {
                    Some(l) => {
                        l.helpful += 1;
                        l.vote_from(task, source);
                        a.upvoted += 1;
                    }
                    None => a.unknown_ids += 1,
                },
                Op::Downvote { id } => match self.get(*id) {
                    Some(l) => {
                        l.importance -= 1;
                        l.harmful += 1;
                        l.sources.push(source.to_string());
                        a.downvoted += 1;
                    }
                    None => a.unknown_ids += 1,
                },
            }
        }
        let before = self.lessons.len();
        self.lessons.retain(|l| l.importance > 0);
        a.deleted = before - self.lessons.len();
        a
    }

    fn get(&mut self, id: u32) -> Option<&mut Lesson> {
        self.lessons.iter_mut().find(|l| l.id == id)
    }

    /// The lessons file: lessons backed by at least `min_tasks` different
    /// tasks, most important first, within `max_chars`.
    pub fn render(&self, max_chars: usize, min_tasks: usize) -> String {
        let mut sorted: Vec<&Lesson> = self
            .lessons
            .iter()
            .filter(|l| l.tasks.len() >= min_tasks)
            .collect();
        sorted.sort_by(|a, b| b.importance.cmp(&a.importance).then(a.id.cmp(&b.id)));
        let mut out = String::new();
        for l in sorted {
            let line = format!("- {}\n", l.text);
            if out.len() + line.len() > max_chars {
                break;
            }
            out.push_str(&line);
        }
        out
    }
}

fn clean(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(MAX_LESSON_CHARS).collect()
}

/// Same words for the most part (Jaccard similarity of lowercase words).
fn similar(a: &str, b: &str) -> bool {
    let words = |s: &str| {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    };
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return false;
    }
    let inter = x.intersection(&y).count() as f64;
    let union = x.union(&y).count() as f64;
    inter / union >= 0.8
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOHUP: &str = "Start servers with nohup so they outlive the command.";

    #[test]
    fn importance_counts_once_per_task() {
        let mut s = Store::default();
        let a = s.apply(
            &[
                Op::Add { text: NOHUP.into() },
                Op::Add {
                    text: "Check installed tool versions before writing code.".into(),
                },
                Op::Add {
                    text: "start servers with nohup, so they outlive the command".into(),
                },
            ],
            "t1",
            "t1/a",
        );
        assert_eq!((a.added, a.duplicates), (2, 1));
        assert_eq!(s.lessons[0].importance, 2);

        let a = s.apply(
            &[
                Op::Upvote { id: 1 },
                Op::Downvote { id: 2 },
                Op::Downvote { id: 9 },
            ],
            "t2",
            "t2/a",
        );
        assert_eq!((a.upvoted, a.downvoted, a.unknown_ids), (1, 1, 1));
        assert_eq!(s.lessons[0].importance, 3);
        // A second downvote takes lesson 2 to 0: it is deleted.
        let a = s.apply(&[Op::Downvote { id: 2 }], "t3", "t3/a");
        assert_eq!(a.deleted, 1);
        assert_eq!(s.lessons.len(), 1);

        let a = s.apply(
            &[Op::Edit {
                id: 1,
                text: "Start long-running servers with nohup.".into(),
            }],
            "t4",
            "t4/a",
        );
        assert_eq!(a.edited, 1);
        assert_eq!(s.lessons[0].importance, 4);
        assert_eq!(
            s.render(1000, 1),
            "- Start long-running servers with nohup.\n"
        );
    }

    #[test]
    fn repeated_runs_of_one_task_count_once() {
        let mut s = Store::default();
        s.apply(&[Op::Add { text: NOHUP.into() }], "t1", "t1/a");
        s.apply(&[Op::Upvote { id: 1 }], "t1", "t1/b");
        s.apply(&[Op::Upvote { id: 1 }], "t1", "t1/c");
        let l = &s.lessons[0];
        assert_eq!((l.importance, l.helpful, l.sources.len()), (2, 2, 3));
        // One task is not enough evidence to show it to the agent.
        assert_eq!(s.render(1000, 2), "");
        s.apply(&[Op::Upvote { id: 1 }], "t2", "t2/a");
        assert_eq!(s.lessons[0].importance, 3);
        assert_eq!(s.render(1000, 2), format!("- {NOHUP}\n"));
    }

    #[test]
    fn render_respects_the_size_limit_and_order() {
        let mut s = Store::default();
        s.apply(
            &[
                Op::Add { text: "low".into() },
                Op::Add {
                    text: "high priority lesson".into(),
                },
            ],
            "t",
            "t/a",
        );
        s.apply(&[Op::Upvote { id: 2 }], "u", "u/a");
        assert_eq!(s.render(1000, 1), "- high priority lesson\n- low\n");
        assert_eq!(s.render(25, 1), "- high priority lesson\n");
    }

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x/lessons.json");
        let mut s = Store::default();
        s.apply(
            &[Op::Add {
                text: "a lesson".into(),
            }],
            "t",
            "t/a",
        );
        s.save(&path).unwrap();
        assert_eq!(Store::load(&path).unwrap(), s);
        assert_eq!(
            Store::load(&dir.path().join("none.json")).unwrap(),
            Store::default()
        );
    }
}
