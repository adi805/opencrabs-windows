//! Mechanical evidence footer for a turn's final answer (FR-007, #1880).
//!
//! Shared by Discord and Telegram so the wording, the category buckets, and
//! the phantom-safety property cannot drift between the two surfaces
//! (NFR-002, NFR-003).
//!
//! The input is a list of tool NAMES taken from what each channel already
//! collected out of `ProgressEvent::ToolStarted` — real executions in the
//! tool loop. Nothing here reads model output, so the line can never name a
//! tool the turn did not run (AC-014, AC-015).
//!
//! The names are folded into category counts (`baca 2 file · jalan 3
//! perintah`) rather than printed verbatim: identifiers are machine
//! vocabulary, and a count per bucket is what the reader is actually
//! asking — how much work happened. Because the buckets are bounded, the
//! line cannot grow into a wall, so the old `+N more` cap is gone.
//!
//! Callers append the line at the CHANNEL layer, after the agent returned.
//! The phantom gate inspects the model's own output, so it never sees this
//! text (AC-016). Belt and braces: the rendered line is pinned by test
//! against every language's `executed_framings`.

/// Header of the evidence footer. Kept as a constant so the phantom-safety
/// test pins the SAME string the renderers emit, instead of a copy that can
/// drift (AC-016).
pub(crate) const EVIDENCE_HEADER: &str = "🛠️";

/// The bucket a tool's action falls into. Declaration order IS the render
/// order, so the footer reads the same way every turn regardless of the
/// order the tools actually ran in.
#[derive(Clone, Copy)]
enum Category {
    Read,
    Run,
    Write,
    Other,
}

impl Category {
    /// One slot per variant, so the count array stays in step with the enum.
    const COUNT: usize = 4;

    /// Tool name -> bucket.
    ///
    /// An unrecognised tool lands in `Category::Other` rather than being
    /// dropped: the footer's job is to account for every action the turn
    /// took, and a newly added tool must not silently vanish from the
    /// receipt.
    fn of(name: &str) -> Self {
        match name {
            "read_file" | "ls" | "glob" | "grep" => Self::Read,
            "bash" => Self::Run,
            "write_file" | "edit_file" | "write_opencrabs_file" | "hashline_edit" => Self::Write,
            _ => Self::Other,
        }
    }

    /// Rendered segment for this bucket, e.g. `baca 2 file`. The noun stays
    /// fixed per bucket; there is no pluralisation branch.
    fn phrase(self, count: usize) -> String {
        match self {
            Self::Read => format!("baca {count} file"),
            Self::Run => format!("jalan {count} perintah"),
            Self::Write => format!("tulis {count} file"),
            Self::Other => format!("lainnya {count}"),
        }
    }
}

/// Category counts, rendered as the footer.
///
/// `None` when the turn ran no tools: an empty or invented line is worse
/// than no line at all (AC-015).
pub(crate) fn evidence_line<'a, I>(names: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    // Unlike the old dedup-by-name, repetition COUNTS: four `bash` calls are
    // `jalan 4 perintah`, which is what the reader wants to know. Only an
    // empty turn yields no line.
    let mut counts = [0usize; Category::COUNT];
    let mut total = 0usize;
    for n in names {
        counts[Category::of(n) as usize] += 1;
        total += 1;
    }
    if total == 0 {
        return None;
    }

    // A bucket with zero actions is omitted, so a read-only turn renders as
    // `🛠️ baca 2 file` rather than a row of zeros. The array is the render
    // order and must stay in step with the enum.
    let mut parts: Vec<String> = Vec::new();
    for cat in [Category::Read, Category::Run, Category::Write, Category::Other] {
        let n = counts[cat as usize];
        if n > 0 {
            parts.push(cat.phrase(n));
        }
    }
    Some(format!("{EVIDENCE_HEADER} {}", parts.join(" · ")))
}
