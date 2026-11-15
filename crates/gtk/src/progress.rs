//! What the app knows about a change that is running: its steps and their
//! state, how far it is, what was printed and how it ended. It is fed the
//! lines `hwtool ... --progress-json` prints and has no widgets, so it is
//! tested without a display.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub title: String,
    pub state: StepState,
    /// How far the running step is, 0 to 1, when the step reports it.
    pub fraction: Option<f64>,
    /// What the step is doing right now, or why it failed.
    pub detail: String,
}

impl Step {
    fn new(title: &str) -> Self {
        Step {
            title: title.to_string(),
            state: StepState::Waiting,
            fraction: None,
            detail: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Running,
    Done {
        reboot: bool,
    },
    Failed {
        /// The step that failed, counting from 0 over all steps.
        step: Option<usize>,
        error: String,
    },
}

/// The steps of one or more hwtool commands run one after the other.
#[derive(Debug, Clone)]
pub struct Model {
    pub steps: Vec<Step>,
    /// Number of steps of each command.
    counts: Vec<usize>,
    /// The command that is running.
    command: usize,
    /// hwtool reported its plan: the command got past the password.
    pub started: bool,
    /// The running command sent its `done` event.
    done_seen: bool,
    reboot: bool,
    pub outcome: Outcome,
    /// Everything printed, for the output view.
    pub log: Vec<String>,
    /// The last line printed.
    pub last_line: String,
    /// The log files of the commands that ran.
    pub log_files: Vec<String>,
    /// What the running command wrote to standard error.
    errors: Vec<String>,
    /// Cleanup steps hwtool ran after a failure, with their result.
    pub cleanup: Vec<(String, StepState)>,
}

fn state_of(v: &Value) -> Option<StepState> {
    match v["state"].as_str()? {
        "running" => Some(StepState::Running),
        "done" => Some(StepState::Done),
        "failed" => Some(StepState::Failed),
        _ => None,
    }
}

impl Model {
    /// `plans` holds the step titles of each command, from its dry run.
    pub fn new(plans: &[Vec<String>]) -> Self {
        Model {
            steps: plans.iter().flatten().map(|t| Step::new(t)).collect(),
            counts: plans.iter().map(Vec::len).collect(),
            command: 0,
            started: false,
            done_seen: false,
            reboot: false,
            outcome: Outcome::Running,
            log: Vec::new(),
            last_line: String::new(),
            log_files: Vec::new(),
            errors: Vec::new(),
            cleanup: Vec::new(),
        }
    }

    /// Index of the first step of the running command.
    fn offset(&self) -> usize {
        self.counts.iter().take(self.command).sum()
    }

    /// The step `n` (counting from 1) of the running command.
    fn step_mut(&mut self, n: u64) -> Option<&mut Step> {
        let n = usize::try_from(n).ok()?;
        let count = *self.counts.get(self.command)?;
        if n == 0 || n > count {
            return None;
        }
        let at = self.offset() + n - 1;
        self.steps.get_mut(at)
    }

    fn print(&mut self, line: &str) {
        let line = line.trim_end();
        self.log.push(line.to_string());
        if !line.trim().is_empty() {
            self.last_line = line.trim().to_string();
        }
    }

    fn fail(&mut self, step: Option<usize>, error: String) {
        if matches!(self.outcome, Outcome::Running) {
            self.outcome = Outcome::Failed { step, error };
        }
    }

    /// Mark a step as failed, unless it already says why it failed.
    fn fail_step(&mut self, step: Option<usize>, error: &str) {
        let Some(i) = step else {
            return;
        };
        if let Some(s) = self.steps.get_mut(i) {
            if s.state != StepState::Failed {
                s.state = StepState::Failed;
                s.detail = error.to_string();
            }
        }
    }

    /// The next command starts.
    pub fn begin(&mut self, command: usize) {
        self.command = command;
        self.started = false;
        self.done_seen = false;
        self.errors.clear();
    }

    /// One line of the command's standard output: a progress event, or
    /// (from anything that is not hwtool, such as pkexec) plain text.
    pub fn line(&mut self, text: &str) {
        let v: Value = match serde_json::from_str(text.trim()) {
            Ok(v) => v,
            Err(_) => {
                self.print(text);
                return;
            }
        };
        match v["event"].as_str() {
            Some("plan") => self.plan(&v),
            Some("step") => self.step(&v),
            Some("progress") => self.progress(&v),
            Some("cleanup") => {
                let title = v["title"].as_str().unwrap_or("").to_string();
                if let Some(state) = state_of(&v) {
                    let open = self
                        .cleanup
                        .iter()
                        .rposition(|(t, s)| *t == title && *s == StepState::Running);
                    match open {
                        Some(i) => self.cleanup[i].1 = state,
                        None => self.cleanup.push((title, state)),
                    }
                }
            }
            Some("log") => {
                if let Some(l) = v["line"].as_str() {
                    self.print(l);
                }
            }
            Some("done") => self.done(&v),
            // An event of a newer hwtool: nothing to show.
            Some(_) => {}
            None => self.print(text),
        }
    }

    fn plan(&mut self, v: &Value) {
        self.started = true;
        let titles: Vec<&str> = v["steps"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        // The fingerprint makes the plan the confirmed one, so this only
        // differs if the dry run could not be read. Show what really runs.
        if self.counts.get(self.command) != Some(&titles.len()) {
            while self.counts.len() <= self.command {
                self.counts.push(0);
            }
            let from = self.offset();
            let to = from + self.counts[self.command];
            self.steps
                .splice(from..to, titles.iter().map(|t| Step::new(t)));
            self.counts[self.command] = titles.len();
        }
    }

    fn step(&mut self, v: &Value) {
        self.started = true;
        let (Some(n), Some(state)) = (v["step"].as_u64(), state_of(v)) else {
            return;
        };
        let error = v["error"].as_str().unwrap_or("").to_string();
        let Some(s) = self.step_mut(n) else {
            return;
        };
        s.state = state;
        match state {
            StepState::Running | StepState::Waiting => {}
            StepState::Done => {
                s.fraction = None;
                s.detail.clear();
            }
            StepState::Failed => s.detail = error,
        }
    }

    fn progress(&mut self, v: &Value) {
        let Some(n) = v["step"].as_u64() else {
            return;
        };
        let Some(s) = self.step_mut(n) else {
            return;
        };
        if s.state != StepState::Running {
            return;
        }
        let fraction = match (
            v["percent"].as_f64(),
            v["bytes"].as_f64(),
            v["total"].as_f64(),
        ) {
            (Some(p), _, _) => Some(p / 100.0),
            (None, Some(b), Some(t)) if t > 0.0 => Some(b / t),
            _ => None,
        };
        if let Some(f) = fraction {
            s.fraction = Some(f.clamp(0.0, 1.0));
        }
        if let Some(d) = v["detail"].as_str() {
            s.detail = d.to_string();
        }
    }

    fn done(&mut self, v: &Value) {
        self.done_seen = true;
        if let Some(l) = v["log"].as_str() {
            self.log_files.push(l.to_string());
        }
        if v["ok"].as_bool() == Some(true) {
            self.reboot |= v["reboot"].as_bool() == Some(true);
            return;
        }
        let offset = self.offset();
        let step = v["failed_step"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n >= 1)
            .map(|n| offset + n - 1)
            .or(self.running());
        let error = v["error"]
            .as_str()
            .unwrap_or("hwtool gave no reason")
            .to_string();
        self.fail_step(step, &error);
        self.fail(step, error);
    }

    /// One line of the command's standard error: pkexec's refusal, or why
    /// hwtool stopped before it ran anything.
    pub fn error_line(&mut self, text: &str) {
        let line = text.trim_end();
        if !line.trim().is_empty() {
            self.errors.push(line.to_string());
        }
        self.print(text);
    }

    /// The command could not be started at all.
    pub fn spawn_failed(&mut self, why: &str) {
        self.print(why);
        self.fail(None, why.to_string());
    }

    /// The command ended with exit status `code` (None: killed by a
    /// signal). `last` says no further command follows.
    pub fn exit(&mut self, code: Option<i32>, last: bool) {
        if code == Some(0) && self.done_seen {
            if last && matches!(self.outcome, Outcome::Running) {
                self.outcome = Outcome::Done {
                    reboot: self.reboot,
                };
            }
            return;
        }
        // It ended without saying how it went: pkexec refused, hwtool
        // stopped before the plan, or it was killed.
        let running = self.running();
        let mut error = self.errors.join("\n");
        if error.is_empty() {
            error = match code {
                Some(0) => "hwtool ended without reporting a result".to_string(),
                Some(c) => format!("hwtool ended with exit status {c}"),
                None => "hwtool was stopped by a signal".to_string(),
            };
        }
        self.fail_step(running, &error);
        self.fail(running, error);
    }

    /// The reader ended without an exit status (it should not).
    pub fn lost(&mut self) {
        let running = self.running();
        self.fail(running, "lost contact with hwtool".to_string());
    }

    pub fn finished(&self) -> bool {
        !matches!(self.outcome, Outcome::Running)
    }

    /// Index of the running step.
    pub fn running(&self) -> Option<usize> {
        self.steps
            .iter()
            .position(|s| s.state == StepState::Running)
    }

    /// Overall progress, 0 to 1: steps done plus the running step's share.
    pub fn fraction(&self) -> f64 {
        if matches!(self.outcome, Outcome::Done { .. }) || self.steps.is_empty() {
            return if self.finished() { 1.0 } else { 0.0 };
        }
        let done: f64 = self
            .steps
            .iter()
            .map(|s| match s.state {
                StepState::Done => 1.0,
                StepState::Running => s.fraction.unwrap_or(0.0),
                _ => 0.0,
            })
            .sum();
        done / self.steps.len() as f64
    }

    /// Nothing says how far the current activity is: before the first step
    /// (the password dialog), or in a step that reports no numbers.
    pub fn indeterminate(&self) -> bool {
        if self.finished() {
            return false;
        }
        match self.running() {
            Some(i) => self.steps[i].fraction.is_none(),
            None => !self.started,
        }
    }

    /// One line on where the change is.
    pub fn headline(&self) -> String {
        let total = self.steps.len();
        match &self.outcome {
            Outcome::Done { .. } => "Done".to_string(),
            Outcome::Failed { step: Some(i), .. } => match self.steps.get(*i) {
                Some(s) => format!("Step {} of {total} failed: {}", i + 1, s.title),
                None => "The change failed".to_string(),
            },
            Outcome::Failed { step: None, .. } => {
                if self.steps.iter().any(|s| s.state != StepState::Waiting) {
                    "The change failed".to_string()
                } else {
                    "Nothing was changed".to_string()
                }
            }
            Outcome::Running => match self.running() {
                // The step's title is in the list, beside the spinner.
                Some(i) => match self.steps[i].fraction {
                    Some(f) => format!("Step {} of {total}, {:.0} %", i + 1, f * 100.0),
                    None => format!("Step {} of {total}", i + 1),
                },
                None if !self.started => "Waiting for permission to make the change".to_string(),
                None => "Working".to_string(),
            },
        }
    }

    /// The line under the progress bar: what the running step is doing,
    /// or how the change ended.
    pub fn status(&self) -> String {
        let logs = match self.log_files.as_slice() {
            [] => String::new(),
            [one] => format!("Log: {one}"),
            many => format!("Logs: {}", many.join(", ")),
        };
        match &self.outcome {
            Outcome::Running => match self.running() {
                Some(i) if !self.steps[i].detail.is_empty() => self.steps[i].detail.clone(),
                _ => self.last_line.clone(),
            },
            Outcome::Done { reboot } => {
                let mut s = String::new();
                if *reboot {
                    s.push_str("Reboot for the change to take effect.\n");
                }
                s.push_str(&logs);
                s.trim_end().to_string()
            }
            Outcome::Failed { step, error } => {
                // The step's own error; hwtool's whole message repeats the
                // step and the cleanup, which are shown beside it.
                let mut s = step
                    .and_then(|i| self.steps.get(i))
                    .map(|s| s.detail.clone())
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| error.clone());
                for (title, state) in &self.cleanup {
                    let how = match state {
                        StepState::Done => "done",
                        StepState::Failed => "FAILED",
                        _ => "not finished",
                    };
                    s.push_str(&format!("\nCleanup {how}: {title}"));
                }
                if !logs.is_empty() {
                    s.push('\n');
                    s.push_str(&logs);
                }
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plans(p: &[&[&str]]) -> Vec<Vec<String>> {
        p.iter()
            .map(|s| s.iter().map(|t| t.to_string()).collect())
            .collect()
    }

    fn states(m: &Model) -> Vec<StepState> {
        m.steps.iter().map(|s| s.state).collect()
    }

    use StepState::{Done, Failed, Running, Waiting};

    #[test]
    fn a_run_goes_step_by_step() {
        let mut m = Model::new(&plans(&[&["Back up", "Write"]]));
        m.begin(0);
        assert_eq!(m.headline(), "Waiting for permission to make the change");
        assert!(m.indeterminate());
        assert_eq!(m.fraction(), 0.0);
        m.line(r#"{"event":"plan","title":"Enable","steps":["Back up","Write"],"reboot":true}"#);
        assert!(m.started);
        m.line(r#"{"event":"step","step":1,"of":2,"title":"Back up","state":"running"}"#);
        m.line(r#"{"event":"log","line":"[1/2] Back up"}"#);
        assert_eq!(states(&m), [Running, Waiting]);
        assert_eq!(m.headline(), "Step 1 of 2");
        assert_eq!(m.status(), "[1/2] Back up");
        assert!(m.indeterminate());
        m.line(r#"{"event":"step","step":1,"of":2,"title":"Back up","state":"done"}"#);
        assert_eq!(m.fraction(), 0.5);
        m.line(r#"{"event":"step","step":2,"of":2,"title":"Write","state":"running"}"#);
        m.line(r#"{"event":"step","step":2,"of":2,"title":"Write","state":"done"}"#);
        m.line(r#"{"event":"done","ok":true,"log":"/var/log/hwtool/t.log","reboot":true}"#);
        // Still running until the process has ended.
        assert!(!m.finished());
        m.exit(Some(0), true);
        assert_eq!(states(&m), [Done, Done]);
        assert_eq!(m.outcome, Outcome::Done { reboot: true });
        assert_eq!(m.fraction(), 1.0);
        assert!(!m.indeterminate());
        assert_eq!(m.headline(), "Done");
        assert_eq!(
            m.status(),
            "Reboot for the change to take effect.\nLog: /var/log/hwtool/t.log"
        );
        assert_eq!(m.log, ["[1/2] Back up"]);
    }

    #[test]
    fn a_step_reports_its_own_progress() {
        let mut m = Model::new(&plans(&[&["Refresh", "Install", "Check", "Report"]]));
        m.begin(0);
        m.line(r#"{"event":"step","step":1,"of":4,"title":"Refresh","state":"done"}"#);
        m.line(r#"{"event":"step","step":2,"of":4,"title":"Install","state":"running"}"#);
        assert!(m.indeterminate());
        m.line(r#"{"event":"progress","step":2,"percent":50.0,"detail":"Unpacking linux-image"}"#);
        assert!(!m.indeterminate());
        assert_eq!(m.steps[1].fraction, Some(0.5));
        assert_eq!(m.fraction(), 1.5 / 4.0);
        assert_eq!(m.headline(), "Step 2 of 4, 50 %");
        assert_eq!(m.status(), "Unpacking linux-image");
        // Bytes without a percentage.
        m.line(r#"{"event":"progress","step":2,"bytes":768,"total":1024}"#);
        assert_eq!(m.steps[1].fraction, Some(0.75));
        assert_eq!(m.status(), "Unpacking linux-image");
        // Progress of a step that is not running is ignored.
        m.line(r#"{"event":"progress","step":3,"percent":10.0}"#);
        m.line(r#"{"event":"progress","step":9,"percent":10.0}"#);
        assert_eq!(m.steps[2].fraction, None);
        m.line(r#"{"event":"step","step":2,"of":4,"title":"Install","state":"done"}"#);
        assert_eq!(m.steps[1].fraction, None);
        assert_eq!(m.steps[1].detail, "");
        assert_eq!(m.fraction(), 0.5);
    }

    #[test]
    fn a_failed_step_is_named_with_its_error() {
        let mut m = Model::new(&plans(&[&["Back up", "Install", "Check"]]));
        m.begin(0);
        m.line(r#"{"event":"step","step":1,"of":3,"title":"Back up","state":"done"}"#);
        m.line(r#"{"event":"step","step":2,"of":3,"title":"Install","state":"running"}"#);
        m.line(r#"{"event":"step","step":2,"of":3,"title":"Install","state":"failed","error":"apt-get exited with 100"}"#);
        m.line(r#"{"event":"cleanup","title":"Put back","state":"running"}"#);
        m.line(r#"{"event":"cleanup","title":"Put back","state":"done"}"#);
        m.line(r#"{"event":"cleanup","title":"Purge","state":"running"}"#);
        m.line(r#"{"event":"cleanup","title":"Purge","state":"failed","error":"x"}"#);
        m.line(r#"{"event":"done","ok":false,"log":"/var/log/hwtool/t.log","reboot":false,"failed_step":2,"error":"step 2 failed: Install: apt-get exited with 100"}"#);
        m.exit(Some(1), true);
        assert_eq!(states(&m), [Done, Failed, Waiting]);
        assert_eq!(m.steps[1].detail, "apt-get exited with 100");
        assert_eq!(
            m.outcome,
            Outcome::Failed {
                step: Some(1),
                error: "step 2 failed: Install: apt-get exited with 100".into()
            }
        );
        assert_eq!(m.headline(), "Step 2 of 3 failed: Install");
        assert_eq!(
            m.status(),
            "apt-get exited with 100\nCleanup done: Put back\n\
             Cleanup FAILED: Purge\nLog: /var/log/hwtool/t.log"
        );
        assert!(!m.indeterminate());
        assert!((m.fraction() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_refused_password_changes_nothing() {
        let mut m = Model::new(&plans(&[&["Back up", "Write"]]));
        m.begin(0);
        m.error_line("Error executing command as another user: Request dismissed");
        m.exit(Some(126), true);
        assert_eq!(states(&m), [Waiting, Waiting]);
        assert_eq!(m.headline(), "Nothing was changed");
        assert_eq!(
            m.status(),
            "Error executing command as another user: Request dismissed"
        );
        assert_eq!(m.fraction(), 0.0);
    }

    #[test]
    fn a_run_refused_before_its_plan_reports_the_reason() {
        let mut m = Model::new(&plans(&[&["Back up", "Write"]]));
        m.begin(0);
        m.line(r#"{"event":"done","ok":false,"reboot":false,"error":"the plan changed since it was confirmed"}"#);
        m.error_line("hwtool: the plan changed since it was confirmed");
        m.exit(Some(1), true);
        assert_eq!(m.headline(), "Nothing was changed");
        assert_eq!(m.status(), "the plan changed since it was confirmed");
    }

    #[test]
    fn a_process_that_dies_fails_the_running_step() {
        let mut m = Model::new(&plans(&[&["Back up", "Write"]]));
        m.begin(0);
        m.line(r#"{"event":"step","step":1,"of":2,"title":"Back up","state":"running"}"#);
        m.exit(None, true);
        assert_eq!(states(&m), [Failed, Waiting]);
        assert_eq!(m.headline(), "Step 1 of 2 failed: Back up");
        assert_eq!(m.status(), "hwtool was stopped by a signal");
        let mut m = Model::new(&plans(&[&["Back up"]]));
        m.begin(0);
        m.spawn_failed("cannot start pkexec: No such file or directory");
        assert_eq!(m.headline(), "Nothing was changed");
        // A later exit does not replace the first reason.
        m.exit(Some(1), true);
        assert_eq!(m.status(), "cannot start pkexec: No such file or directory");
    }

    #[test]
    fn several_commands_share_one_list() {
        let mut m = Model::new(&plans(&[&["A1", "A2"], &["B1"]]));
        m.begin(0);
        m.line(r#"{"event":"step","step":1,"of":2,"title":"A1","state":"done"}"#);
        m.line(r#"{"event":"step","step":2,"of":2,"title":"A2","state":"done"}"#);
        m.line(r#"{"event":"done","ok":true,"log":"/var/log/hwtool/a.log","reboot":true}"#);
        m.exit(Some(0), false);
        assert!(!m.finished());
        m.begin(1);
        assert!(!m.started);
        m.line(r#"{"event":"step","step":1,"of":1,"title":"B1","state":"running"}"#);
        assert_eq!(states(&m), [Done, Done, Running]);
        assert_eq!(m.headline(), "Step 3 of 3");
        m.line(r#"{"event":"step","step":1,"of":1,"title":"B1","state":"done"}"#);
        m.line(r#"{"event":"done","ok":true,"log":"/var/log/hwtool/b.log","reboot":false}"#);
        m.exit(Some(0), true);
        // One of the commands needs a reboot.
        assert_eq!(m.outcome, Outcome::Done { reboot: true });
        assert_eq!(
            m.status(),
            "Reboot for the change to take effect.\nLogs: /var/log/hwtool/a.log, /var/log/hwtool/b.log"
        );
    }

    #[test]
    fn the_plan_event_wins_over_an_unreadable_dry_run() {
        let mut m = Model::new(&plans(&[&[], &["B1"]]));
        m.begin(0);
        m.line(r#"{"event":"plan","title":"T","steps":["A1","A2"],"reboot":false}"#);
        m.line(r#"{"event":"step","step":2,"of":2,"title":"A2","state":"running"}"#);
        let titles: Vec<&str> = m.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["A1", "A2", "B1"]);
        assert_eq!(states(&m), [Waiting, Running, Waiting]);
    }

    #[test]
    fn text_that_is_no_event_is_output() {
        let mut m = Model::new(&plans(&[&["A"]]));
        m.begin(0);
        m.line("plain text from somewhere");
        m.line(r#"{"event":"something new","x":1}"#);
        m.line(r#"{"no":"event"}"#);
        m.line("");
        assert_eq!(
            m.log,
            ["plain text from somewhere", r#"{"no":"event"}"#, ""]
        );
        assert_eq!(m.last_line, r#"{"no":"event"}"#);
    }
}
