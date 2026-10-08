//! The diagnostic review injected when the tool loop notices it is circling
//! (PRD FR-001).
//!
//! The existing guards all notice repetition and then act on it: `tool_repeat`
//! nudges, the dominant-repeat guard in `tool_loop` nudges and then breaks, and
//! `loop_break` hands the turn to the next provider. Every one of them says
//! WHAT was repeated. None of them says anything about WHETHER the work is
//! progressing, so the same loop can re-form on the fallback provider — the
//! exact failure the fallback chain exists to absorb and cannot diagnose.
//!
//! This adds the missing half: a fixed six-question review, injected alongside
//! the existing correction, so the model has to reach an assessment before its
//! next call instead of only being told that it repeated itself.
//!
//! Triggers are COUNT-based, not clock-based. A clock measures how long the
//! loop has been running; a count measures how much it has cost, and the cost
//! is what the operator pays. The reference implementation
//! (`vxcontrol/pentagi`'s execution monitor) counts the same way.
//!
//! Three invariants, the first two inherited from the nudges it sits beside:
//! it never ends the turn, it never suppresses a call, and at most one block
//! fires per turn — a model that ignored the first review will not read a
//! second one.
//!
//! Pure so the wording and the cap are testable without a provider.

/// Same-tool calls within one turn before the review fires.
pub(crate) const SAME_TOOL_TRIGGER: u32 = 5;

/// Total tool calls within one turn before the review fires.
///
/// The backstop for the loop that never repeats one call but never converges
/// either: a model cycling through six different tools looks fresh to every
/// per-call detector and still burns the whole quota.
pub(crate) const TOTAL_CALL_TRIGGER: u32 = 10;

/// The six questions, in a fixed order.
///
/// Fixed on purpose. The value of the review is that the same six are asked
/// every time, so the answer is comparable across turns; a question list that
/// varied with the trigger would produce an assessment nobody could compare.
pub(crate) const DIAGNOSTIC_QUESTIONS: [&str; 6] = [
    "Is the work actually progressing, or is it circling?",
    "Is the same failure repeating, and if so what is the invariant?",
    "Is the direction still right, or has the goal drifted from the task?",
    "What is one concrete alternative approach that has not been tried?",
    "Is the goal impossible as defined, rather than merely hard to reach?",
    "What are the next steps, stated as actions rather than intentions?",
];

/// The escape hatch, shared with the phantom nudges for the same reason.
///
/// Without it the review becomes a loop of its own: a model that genuinely
/// finished, and said so, gets asked to assess progress, calls something
/// pointless to comply, and is reviewed again. Real completion needs an exit
/// that is not a tool call.
const FINISHED_ESCAPE: &str = "If the work is genuinely finished and you have already reported \
     it, reply with a short confirmation and stop; do not run extra tool calls to re-verify it.";

/// Why the review fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TriggerKind {
    /// One tool called `count` times in a row inside this turn.
    SameTool { tool: String, count: u32 },
    /// This turn reached `count` tool calls in total.
    TotalCalls { count: u32 },
}

/// What the tally concluded about the call just observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentorVerdict {
    /// Nothing to say.
    Quiet,
    /// The trigger fired; the review text is ready to inject.
    Diagnose(String),
}

/// Tracks one turn's tool calls.
#[derive(Debug, Default)]
pub(crate) struct LoopTally {
    last_tool: Option<String>,
    same_tool: u32,
    total: u32,
    fired: bool,
}

impl LoopTally {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record one tool call and report whether the review should fire.
    ///
    /// The `fired` latch is set the moment a block is produced and is never
    /// cleared by later calls, which is what makes the cap a per-turn cap
    /// rather than a per-burst one (AC-005).
    pub(crate) fn observe(&mut self, tool_name: &str) -> MentorVerdict {
        self.total += 1;
        if self.last_tool.as_deref() == Some(tool_name) {
            self.same_tool += 1;
        } else {
            self.last_tool = Some(tool_name.to_string());
            self.same_tool = 1;
        }
        if self.fired {
            return MentorVerdict::Quiet;
        }
        if self.same_tool >= SAME_TOOL_TRIGGER {
            self.fired = true;
            return MentorVerdict::Diagnose(mentor_block(&TriggerKind::SameTool {
                tool: tool_name.to_string(),
                count: self.same_tool,
            }));
        }
        if self.total >= TOTAL_CALL_TRIGGER {
            self.fired = true;
            return MentorVerdict::Diagnose(mentor_block(&TriggerKind::TotalCalls {
                count: self.total,
            }));
        }
        MentorVerdict::Quiet
    }

    /// Forget the turn so far.
    ///
    /// Called when a turn is replayed, for the same reason `ToolRepeatTracker`
    /// resets: a retry or a fallback re-sends the failed attempt's calls, and
    /// those replayed copies must not stack onto the count for calls the model
    /// made once.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Forget the replayed calls but keep the one-per-turn latch.
    ///
    /// What a provider swap wants. The replayed copies must not stack onto the
    /// count (so the counters clear), but the cap is per TURN, not per attempt
    /// (so the latch stays): a second provider does not earn a second review
    /// for work the turn already had reviewed.
    pub(crate) fn reset_counts(&mut self) {
        self.last_tool = None;
        self.same_tool = 0;
        self.total = 0;
    }

    /// Total tool calls recorded in this turn.
    pub(crate) fn total(&self) -> u32 {
        self.total
    }

    /// Whether the review has already fired this turn.
    pub(crate) fn fired(&self) -> bool {
        self.fired
    }
}

/// The review text for a fired trigger.
///
/// States the count rather than scolding: the number is checkable, and a model
/// can argue with an adjective but not with a tally it produced.
pub(crate) fn mentor_block(kind: &TriggerKind) -> String {
    let why = match kind {
        TriggerKind::SameTool { tool, count } => {
            format!("`{tool}` has now been called {count} times in a row in this turn")
        }
        TriggerKind::TotalCalls { count } => {
            format!("this turn has now issued {count} tool calls")
        }
    };
    let questions = DIAGNOSTIC_QUESTIONS
        .iter()
        .enumerate()
        .map(|(i, q)| format!("{}. {q}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[System: {why}. Repetition is not progress: an identical call returns an identical \
         result, and a long turn is not a productive one. Before the next call, answer these \
         six questions, then act on the answer:\n{questions}\n{FINISHED_ESCAPE}]"
    )
}

/// Observe one whole tool round and return the review to inject, if any.
///
/// The single entry point the loop uses, so the call site stays one line and
/// the trigger logic lives here where it is testable. `tool_name` is the first
/// tool of the round; a round with no tools cannot repeat and is ignored
/// outright.
pub(crate) fn observe_round(tally: &mut LoopTally, tool_name: Option<&str>) -> Option<String> {
    let name = tool_name?;
    match tally.observe(name) {
        MentorVerdict::Diagnose(block) => Some(block),
        MentorVerdict::Quiet => None,
    }
}
