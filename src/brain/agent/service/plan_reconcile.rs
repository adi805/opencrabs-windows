//! Reconcile the plan against the work actually done, on every task
//! transition (PRD FR-002).
//!
//! The measured failure this closes: the plan card rendered 4/9 while the work
//! was 9/9. Five dated incidents are on record. The plan tool verifies a
//! completion that is handed to it, and nothing checks whether a completion
//! that was NEVER handed to it should have been. So the card and the artifacts
//! drift apart and the only sensor is the operator noticing.
//!
//! Two halves, deliberately separate:
//!
//! * [`reconcile`] reads the plan plus a set of OBSERVED FACTS and reports
//!   drift. It is pure, so a verdict is testable without a filesystem.
//! * [`validate_patch`] is the gate every delta op passes before it can touch
//!   the document. It is what makes "never revert a completed task" mechanical
//!   instead of a convention (NFR-002).
//!
//! Evidence, not claims. A task is only auto-completed when every artifact
//! path it declares is observed to exist. A model's own statement that work is
//! done is not evidence -- the project's first value is that claims need
//! receipts -- so [`WorkEvidence::claimed_done`] corroborates but can never on
//! its own produce a patch.
//!
//! Blast radius decides where the patch stops. A plan is forward-only: a task
//! marked complete cannot be un-marked, so a WRONG completion is worse than a
//! missing one, because it hides unfinished work behind a green row. The
//! generator therefore patches only when the state already says work began
//! (`InProgress`) or the turn reported it (`claimed_done`); a bare `Pending`
//! task whose artifacts are on disk produces a finding and a notice, and the
//! agent makes the call. That keeps the sensor on the drift without letting a
//! heuristic close a task nobody did.

use crate::tui::plan::{PlanDocument, PlanTask, TaskStatus};

/// Extensions that make a token in task prose a declared artifact.
///
/// Matched on the suffix alone, so a bare filename counts and a directory
/// mention does not. Deliberately narrow: a token is evidence only when its
/// shape is unambiguous, and `9/9`, `AC-003` or `Lint/Test` must never read as
/// a path.
const ARTIFACT_EXTENSIONS: [&str; 24] = [
    ".rs", ".toml", ".json", ".md", ".txt", ".sh", ".py", ".js", ".ts", ".tsx", ".html", ".css",
    ".yml", ".yaml", ".sql", ".mp4", ".png", ".jpg", ".svg", ".csv", ".lock", ".cfg", ".ini",
    ".log",
];

/// A delta operation on the plan document.
///
/// Deliberately small. The reference implementation carries add/remove/modify/
/// reorder, but a vocabulary is only worth its validation branches when
/// something can construct each op, so this keeps the two the evidence can
/// actually justify: `Modify` for a row the artifacts say is done, and
/// `Remove` for a row a resolved sibling already covers. An op with no signal
/// behind it is a branch nothing can reach, which is a liability in a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchOp {
    /// Drop a task that a resolved sibling already covers.
    Remove { order: usize, reason: String },
    /// Change a task in place. `status` is the only field the generator sets.
    Modify {
        order: usize,
        status: Option<TaskStatus>,
        reason: String,
    },
}

impl PatchOp {
    /// The task order this op targets.
    pub(crate) fn order(&self) -> usize {
        match self {
            PatchOp::Remove { order, .. } | PatchOp::Modify { order, .. } => *order,
        }
    }

    /// The recorded reason, which every op is required to carry.
    pub(crate) fn reason(&self) -> &str {
        match self {
            PatchOp::Remove { reason, .. } | PatchOp::Modify { reason, .. } => reason,
        }
    }

    /// One-line description for the recorded patch log.
    pub(crate) fn describe(&self) -> String {
        match self {
            PatchOp::Remove { order, .. } => format!("remove #{order}"),
            PatchOp::Modify { order, status, .. } => match status {
                Some(s) => format!("modify #{order} -> {s}"),
                None => format!("modify #{order}"),
            },
        }
    }
}

/// What the reconciler noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriftKind {
    /// The declared artifacts exist and the task is still open.
    UnmarkedCompletion,
    /// Two tasks carry the same title.
    DuplicateTitle,
    /// A dependency points at an order no task has.
    DanglingDependency,
}

impl DriftKind {
    /// Short label for the notice the operator reads.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            DriftKind::UnmarkedCompletion => "work done, row still open",
            DriftKind::DuplicateTitle => "duplicate row",
            DriftKind::DanglingDependency => "dependency cannot resolve",
        }
    }
}

/// One observation, with the order and the evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub order: usize,
    pub kind: DriftKind,
    pub reason: String,
}

/// What [`reconcile`] concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReconcileVerdict {
    /// The plan and the artifacts agree.
    NoDrift,
    /// Drift found. `patch` holds the ops the evidence supports; it is empty
    /// when every finding is report-only (AC-004 pins the empty case).
    Drift {
        findings: Vec<Finding>,
        patch: Vec<PatchOp>,
    },
}

/// The observed facts a reconciliation reads.
///
/// Facts, not opinions: a path is here because it was stat-ed, and an order is
/// in `claimed_done` because the turn's own output named it. Nothing in this
/// struct is inferred from the plan it is compared against.
#[derive(Debug, Clone, Default)]
pub(crate) struct WorkEvidence {
    /// Paths observed to exist on disk.
    pub present_paths: Vec<String>,
    /// Orders the turn reported as done. Corroborates; never sufficient alone.
    pub claimed_done: Vec<usize>,
}

/// Compare a plan against the observed work and report drift.
///
/// Pure: every fact comes from `evidence`, so a test can pin a verdict without
/// touching a filesystem.
pub(crate) fn reconcile(plan: &PlanDocument, evidence: &WorkEvidence) -> ReconcileVerdict {
    let present: Vec<String> = evidence
        .present_paths
        .iter()
        .map(|p| normalize_path(p))
        .collect();
    let mut findings = Vec::new();
    let mut patch = Vec::new();

    // Artifacts on disk while the row still reads open: the measured class.
    for task in &plan.tasks {
        if is_resolved(&task.status) {
            continue;
        }
        let declared = declared_artifacts(task);
        if declared.is_empty() {
            continue;
        }
        // Every declared path must be observed, not merely one of them: a
        // task that produces two artifacts is not done when the first lands.
        if !declared
            .iter()
            .all(|d| present.contains(&normalize_path(d)))
        {
            continue;
        }
        let corroborated = evidence.claimed_done.contains(&task.order);
        let mut reason = format!(
            "all {} declared artifact(s) present ({}); task is still {}",
            declared.len(),
            declared.join(", "),
            task.status
        );
        if corroborated {
            reason.push_str(" and this turn reported it done");
        }
        findings.push(Finding {
            order: task.order,
            kind: DriftKind::UnmarkedCompletion,
            reason: reason.clone(),
        });
        // Only patch where the state already says work began, or where the
        // turn said so. A bare Pending row is reported, never closed.
        let may_patch = matches!(task.status, TaskStatus::InProgress) || corroborated;
        if may_patch {
            patch.push(PatchOp::Modify {
                order: task.order,
                status: Some(TaskStatus::Completed),
                reason,
            });
        }
    }

    // Same title twice: the later row is the one that duplicates.
    for (i, task) in plan.tasks.iter().enumerate() {
        let dup_of = plan.tasks[..i]
            .iter()
            .find(|t| t.title.trim().eq_ignore_ascii_case(task.title.trim()));
        if let Some(first) = dup_of {
            let mut reason = format!(
                "title '{}' duplicates #{}, which is {}",
                task.title, first.order, first.status
            );
            findings.push(Finding {
                order: task.order,
                kind: DriftKind::DuplicateTitle,
                reason: reason.clone(),
            });
            // Drop the duplicate only when the sibling it duplicates is
            // already resolved: then the work is genuinely covered and the
            // open row is a leftover. When neither is resolved the two rows
            // may be two real pieces of work, so the finding is reported and
            // the agent decides -- removing one could delete work nobody did.
            if is_resolved(&first.status) && !is_resolved(&task.status) {
                reason.push_str("; the resolved sibling already covers it");
                patch.push(PatchOp::Remove {
                    order: task.order,
                    reason,
                });
            }
        }
    }

    // A dependency on an order no task has can never unblock.
    let orders: Vec<usize> = plan.tasks.iter().map(|t| t.order).collect();
    for task in &plan.tasks {
        for dep in &task.dependencies {
            let target = dep_order(dep);
            if let Some(target) = target
                && !orders.contains(&target)
            {
                findings.push(Finding {
                    order: task.order,
                    kind: DriftKind::DanglingDependency,
                    reason: format!("depends on #{target}, which no task has"),
                });
            }
        }
    }

    if findings.is_empty() {
        ReconcileVerdict::NoDrift
    } else {
        ReconcileVerdict::Drift { findings, patch }
    }
}

/// Why an op was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RejectReason {
    /// The op names an order the plan does not have.
    UnknownOrder(usize),
    /// The op would move a resolved task back into an open state.
    RevertsResolvedTask {
        order: usize,
        from: String,
        to: String,
    },
    /// The op would delete a task that is already done.
    DropsResolvedTask { order: usize },
    /// The op carries no reason, so nothing would be recorded.
    MissingReason,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectReason::UnknownOrder(o) => write!(f, "task #{o} does not exist"),
            RejectReason::RevertsResolvedTask { order, from, to } => write!(
                f,
                "would move resolved task #{order} from {from} back to {to}"
            ),
            RejectReason::DropsResolvedTask { order } => {
                write!(f, "would drop resolved task #{order}")
            }
            RejectReason::MissingReason => write!(f, "carries no reason to record"),
        }
    }
}

/// The outcome of validating a patch: what may land, and what was refused.
///
/// Partial by design. One refused op costs only itself (the reference
/// implementation's rule), and the refusal is recorded with its reason.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PatchVerdict {
    pub accepted: Vec<PatchOp>,
    pub rejected: Vec<(PatchOp, RejectReason)>,
}

/// The gate every op passes before it can touch the document.
///
/// The two invariants that matter: a resolved task is never reopened, and a
/// resolved task is never dropped. Both are checked here rather than trusted
/// of the caller, which is what makes NFR-002 mechanical.
pub(crate) fn validate_patch(plan: &PlanDocument, patch: &[PatchOp]) -> PatchVerdict {
    let mut verdict = PatchVerdict::default();
    for op in patch {
        let reason = if op.reason().trim().is_empty() {
            Some(RejectReason::MissingReason)
        } else {
            None
        };
        let reject = reason.or_else(|| match op {
            PatchOp::Remove { order, .. } => match plan.get_task_by_order(*order) {
                None => Some(RejectReason::UnknownOrder(*order)),
                Some(t) if is_resolved(&t.status) => {
                    Some(RejectReason::DropsResolvedTask { order: *order })
                }
                Some(_) => None,
            },
            PatchOp::Modify { order, status, .. } => match plan.get_task_by_order(*order) {
                None => Some(RejectReason::UnknownOrder(*order)),
                Some(t) => match status {
                    Some(s) if is_resolved(&t.status) && !is_resolved(s) => {
                        Some(RejectReason::RevertsResolvedTask {
                            order: *order,
                            from: t.status.to_string(),
                            to: s.to_string(),
                        })
                    }
                    _ => None,
                },
            },
        });
        match reject {
            Some(r) => verdict.rejected.push((op.clone(), r)),
            None => verdict.accepted.push(op.clone()),
        }
    }
    verdict
}

/// Apply the accepted ops and return one recorded line per op.
///
/// Every mutation produces a line, so a patch can never be silent (NFR-002).
pub(crate) fn apply_patch(plan: &mut PlanDocument, accepted: &[PatchOp]) -> Vec<String> {
    let mut recorded = Vec::new();
    for op in accepted {
        match op {
            PatchOp::Remove { order, reason } => {
                plan.tasks.retain(|t| t.order != *order);
                recorded.push(format!("{} ({reason})", op.describe()));
            }
            PatchOp::Modify {
                order,
                status,
                reason,
            } => {
                if let Some(task) = plan.get_task_by_order_mut(*order) {
                    if let Some(s) = status {
                        task.status = s.clone();
                    }
                    let stamped = format!("[reconcile] {reason}");
                    task.notes = Some(match task.notes.take() {
                        Some(prev) if !prev.is_empty() => format!("{prev}\n{stamped}"),
                        _ => stamped,
                    });
                    recorded.push(format!("{} ({reason})", op.describe()));
                }
            }
        }
    }
    plan.updated_at = chrono::Utc::now();
    recorded
}

/// Gather the observed facts for a plan by stat-ing what its tasks declare.
///
/// The only I/O in this module, kept apart from [`reconcile`] so the verdict
/// stays testable. A path that cannot be resolved relative to `working_dir` is
/// tried as given, which is what an absolute path in a criterion needs.
pub(crate) fn collect_evidence(plan: &PlanDocument, working_dir: &std::path::Path) -> WorkEvidence {
    let mut present = Vec::new();
    for task in &plan.tasks {
        if is_resolved(&task.status) {
            continue;
        }
        for artifact in declared_artifacts(task) {
            let direct = std::path::Path::new(&artifact);
            let joined = working_dir.join(&artifact);
            if direct.exists() || joined.exists() {
                present.push(artifact);
            }
        }
    }
    WorkEvidence {
        present_paths: present,
        claimed_done: Vec::new(),
    }
}

/// Tokens in a task's own text that name a file it claims to produce.
pub(crate) fn declared_artifacts(task: &PlanTask) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in std::iter::once(&task.description).chain(task.acceptance_criteria.iter()) {
        for raw in text.split_whitespace() {
            let token = clean_token(raw);
            if !looks_like_path(token) {
                continue;
            }
            let normalized = normalize_path(token);
            if !out.iter().any(|e| normalize_path(e) == normalized) {
                out.push(token.to_string());
            }
        }
    }
    out
}

/// A task is resolved once it can no longer be worked on.
fn is_resolved(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Completed | TaskStatus::Skipped)
}

/// Strip the punctuation prose wraps a path in, without eating the suffix.
fn clean_token(token: &str) -> &str {
    token.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"' | '\'' | '(' | ')' | ',' | ';' | '<' | '>' | '[' | ']' | '*'
        )
    })
}

/// Whether a token's shape is unambiguously a file.
fn looks_like_path(token: &str) -> bool {
    if token.len() < 4 || token.contains("://") || token.contains('=') {
        return false;
    }
    if token.starts_with('-') || token.starts_with('#') {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    ARTIFACT_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

/// Compare paths the way a filesystem does on the platforms we build for.
fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// The dependency's target order, when it is expressed as one.
///
/// A UUID dependency is resolved against the live task list at import, so only
/// an index can be checked for dangling by order alone.
fn dep_order(dep: &crate::tui::plan::TaskDep) -> Option<usize> {
    match dep {
        crate::tui::plan::TaskDep::Index(o) => Some(*o),
        crate::tui::plan::TaskDep::Id(_) => None,
    }
}
