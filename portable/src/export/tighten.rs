//! `rec export --tighten`: retime a take so its pacing follows the app instead of the agent
//! that drove it. The cast and the input log say when the screen changed and why; that splits
//! the take into typed segments, a policy table gives each segment its length in the output,
//! and the result is a piecewise-linear map from output time back to take time.

use super::FPS;
use crate::model::{Event, Size, TimedEvent};
use serde::Serialize;

/// Something that happened on the take clock.
#[derive(Debug, Clone, PartialEq)]
pub enum Point {
    /// Input `rec` sent. `chars` is how many characters a `type` sent, which stretches its echo.
    Input { t: f64, chars: usize },
    /// The screen at the end of one export frame differs from the frame before.
    Change { t: f64, kind: Change, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A small edit in place on lines that already had text: a spinner glyph, a ticking counter.
    Minor,
    /// New text, or a redraw too big to be a status tick.
    Content,
}

impl Point {
    fn t(&self) -> f64 {
        match self {
            Point::Input { t, .. } | Point::Change { t, .. } => *t,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Before anything is drawn.
    Lead,
    /// From an input through the echo it causes.
    Typing,
    /// Output arriving that nobody just asked for, or that outlasts the echo.
    Content,
    /// The app is working: only minor changes, ending in output it produced on its own.
    Busy,
    /// Nothing changes until the next input.
    Settled,
    /// Nothing changes until the take ends.
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub kind: Kind,
    pub take: (f64, f64),
    /// For quiet segments (Busy, Settled, End): words on screen that were not on the previous
    /// quiet screen, which sets how long a viewer needs to read it.
    pub words: usize,
}

impl Segment {
    fn len(&self) -> f64 {
        self.take.1 - self.take.0
    }
}

/// Seconds a viewer needs to read `words` new words.
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    pub base: f64,
    pub words_per_s: f64,
    pub min: f64,
    pub max: f64,
}

impl Reading {
    pub fn hold(&self, words: usize) -> f64 {
        (self.base + words as f64 / self.words_per_s).clamp(self.min, self.max)
    }
}

/// Every pacing rule tighten applies, in one place.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Content plays at 1x for this long; whatever runs past it plays at `content_speed`.
    pub content_full: f64,
    pub content_speed: f64,
    /// Busy compresses toward this many seconds, never faster than `busy_max_speed`.
    pub busy_target: f64,
    pub busy_max_speed: f64,
    pub reading: Reading,
    /// How much of the wait before an input stays in, so the viewer sees the input land.
    pub preroll: f64,
    /// The final screen holds at least this long.
    pub end_min: f64,
}

pub const POLICY: Policy = Policy {
    content_full: 3.0,
    content_speed: 2.0,
    busy_target: 2.0,
    busy_max_speed: 12.0,
    reading: Reading {
        base: 0.6,
        words_per_s: 4.0,
        min: 1.2,
        max: 6.0,
    },
    preroll: 0.25,
    end_min: 2.0,
};

/// Changes no bigger than this, on lines that already had text, are status ticks.
const MINOR_CELLS: usize = 24;
const MINOR_ROWS: usize = 2;
/// Points closer than this belong to one burst of activity.
const BURST_GAP: f64 = 0.5;
/// How long after an input its echo can arrive, plus this much per typed character.
const ECHO: f64 = 0.5;
const ECHO_PER_CHAR: f64 = 0.1;

/// Replays the cast and lists every input and every frame whose screen differs from the last.
pub fn analyze(
    output: &[(f64, String)],
    events: &[TimedEvent],
    size: Size,
    duration: f64,
) -> Vec<Point> {
    let mut points: Vec<Point> = events
        .iter()
        .filter(|e| e.t <= duration)
        .filter_map(|e| match &e.event {
            Event::Type { text, .. } => Some(Point::Input {
                t: e.t,
                chars: text.as_ref().map_or(0, |s| s.chars().count()),
            }),
            Event::Key { .. } => Some(Point::Input { t: e.t, chars: 0 }),
            Event::Marker { .. } => None,
        })
        .collect();

    let mut parser = vt100::Parser::new(size.rows, size.cols, 0);
    let mut prev = parser.screen().clone();
    let mut chunks = output.iter().filter(|(t, _)| *t <= duration).peekable();
    // The renderer shows chunk `t` first in frame ceil(t * FPS) - 1; diff at that granularity
    // so a redraw split across writes counts once.
    let frame_of = |t: f64| (t * FPS as f64).ceil() as i64;
    while let Some((t, data)) = chunks.next() {
        let frame = frame_of(*t);
        let mut last = *t;
        parser.process(data.as_bytes());
        while let Some((t, data)) = chunks.next_if(|(t, _)| frame_of(*t) == frame) {
            parser.process(data.as_bytes());
            last = *t;
        }
        let now = parser.screen();
        if let Some(kind) = classify(&prev, now) {
            points.push(Point::Change {
                t: last,
                kind,
                text: now.contents(),
            });
            prev = now.clone();
        }
    }
    points.sort_by(|a, b| {
        a.t().total_cmp(&b.t()).then_with(|| {
            let rank = |p: &Point| matches!(p, Point::Change { .. });
            rank(a).cmp(&rank(b))
        })
    });
    points
}

fn classify(prev: &vt100::Screen, now: &vt100::Screen) -> Option<Change> {
    let (rows, cols) = now.size();
    let mut cells = 0;
    let mut touched = 0;
    let mut onto_blank = false;
    for row in 0..rows {
        let mut row_cells = 0;
        for col in 0..cols {
            if prev.cell(row, col) != now.cell(row, col) {
                row_cells += 1;
            }
        }
        if row_cells > 0 {
            cells += row_cells;
            touched += 1;
            let was_blank = (0..cols).all(|col| {
                prev.cell(row, col)
                    .is_none_or(|c| c.contents().trim().is_empty())
            });
            onto_blank |= was_blank;
        }
    }
    match cells {
        0 => None,
        _ if cells <= MINOR_CELLS && touched <= MINOR_ROWS && !onto_blank => Some(Change::Minor),
        _ => Some(Change::Content),
    }
}

/// Splits the take into segments that cover `0..duration` with no gaps.
pub fn segment(points: &[Point], duration: f64) -> Vec<Segment> {
    // Active points form bursts; minor changes outside an echo window stay inside quiet spans.
    let mut echo_until = f64::NEG_INFINITY;
    let mut active: Vec<(f64, Role)> = Vec::new();
    let mut text_at: Vec<(f64, &str)> = Vec::new();
    for p in points {
        match p {
            Point::Input { t, chars } => {
                echo_until = echo_until.max(t + ECHO + ECHO_PER_CHAR * *chars as f64);
                active.push((*t, Role::Input));
            }
            Point::Change { t, kind, text } => {
                text_at.push((*t, text));
                if *t <= echo_until {
                    active.push((*t, Role::Echo));
                } else if *kind == Change::Content {
                    active.push((*t, Role::Output));
                }
            }
        }
    }

    let mut bursts: Vec<&[(f64, Role)]> = Vec::new();
    let mut rest = &active[..];
    while !rest.is_empty() {
        let n = 1 + rest
            .windows(2)
            .take_while(|w| w[1].0 - w[0].0 < BURST_GAP)
            .count();
        let (burst, tail) = rest.split_at(n);
        bursts.push(burst);
        rest = tail;
    }

    let mut segs: Vec<Segment> = Vec::new();
    let mut add = |kind: Kind, take: (f64, f64)| {
        segs.push(Segment {
            kind,
            take,
            words: 0,
        })
    };
    let mut cursor = 0.0;
    for (n, burst) in bursts.iter().enumerate() {
        let (start, opener) = burst[0];
        let quiet = match (n, opener) {
            (0, _) => Kind::Lead,
            (_, Role::Input) => Kind::Settled,
            _ => Kind::Busy,
        };
        if n == 0 || start > cursor {
            add(quiet, (cursor, start));
        }
        // Typing runs from each input through its echo; the rest of the burst is Content.
        let mut a = start;
        let mut kind = opener.kind();
        for &(t, role) in &burst[1..] {
            if role.kind() != kind {
                add(kind, (a, t));
                (a, kind) = (t, role.kind());
            }
        }
        cursor = burst[burst.len() - 1].0;
        add(kind, (a, cursor));
    }
    match bursts.is_empty() {
        true => add(Kind::Lead, (0.0, duration)),
        false if duration > cursor => add(Kind::End, (cursor, duration)),
        false => {}
    }

    let mut quiet_text = "";
    for seg in segs.iter_mut() {
        if matches!(seg.kind, Kind::Busy | Kind::Settled | Kind::End) {
            let shown = match text_at.partition_point(|(t, _)| *t <= seg.take.0) {
                0 => "",
                n => text_at[n - 1].1,
            };
            seg.words = new_words(quiet_text, shown);
            quiet_text = shown;
        }
    }
    segs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    Echo,
    Output,
}

impl Role {
    fn kind(self) -> Kind {
        match self {
            Role::Input | Role::Echo => Kind::Typing,
            Role::Output => Kind::Content,
        }
    }
}

/// Words in `now` beyond those already in `before`, counted with multiplicity.
fn new_words(before: &str, now: &str) -> usize {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let wordy = |w: &&str| w.chars().any(char::is_alphanumeric);
    for w in before.split_whitespace().filter(wordy) {
        *seen.entry(w).or_default() += 1;
    }
    now.split_whitespace()
        .filter(wordy)
        .filter(|w| match seen.get_mut(w) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .count()
}

/// A run of output time that plays `take` linearly.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    out: f64,
    len: f64,
    take: (f64, f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeMap {
    pieces: Vec<Piece>,
    end: f64,
}

impl TimeMap {
    pub fn identity(duration: f64) -> TimeMap {
        TimeMap {
            pieces: vec![Piece {
                out: 0.0,
                len: duration,
                take: (0.0, duration),
            }],
            end: duration,
        }
    }

    pub fn duration(&self) -> f64 {
        self.pieces.last().map_or(0.0, |p| p.out + p.len)
    }

    /// The take time an output frame at `out` shows.
    pub fn take_time(&self, out: f64) -> f64 {
        let i = self.pieces.partition_point(|p| p.out + p.len <= out);
        let Some(p) = self.pieces.get(i) else {
            return self.end;
        };
        let f = ((out - p.out) / p.len).clamp(0.0, 1.0);
        p.take.0 + f * (p.take.1 - p.take.0)
    }
}

/// What tighten did to one segment, as `rec export` reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edit {
    pub kind: Kind,
    pub take: [f64; 2],
    pub out: [f64; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<usize>,
    /// For Settled and End: the reading time the policy granted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tightened {
    pub duration_raw: f64,
    pub duration: f64,
    pub segments: Vec<Edit>,
}

/// Applies the policy to each segment and joins the pieces into one time map.
pub fn plan(segs: &[Segment], policy: &Policy, duration: f64) -> (TimeMap, Tightened) {
    let mut pieces = Vec::new();
    let mut edits = Vec::new();
    let mut out = 0.0;
    for seg in segs {
        let (a, b) = seg.take;
        let len = seg.len();
        let hold = policy.reading.hold(seg.words);
        let spans: Vec<((f64, f64), f64)> = match seg.kind {
            Kind::Lead => vec![],
            Kind::Typing => vec![((a, b), len)],
            Kind::Content => {
                let extra = (len - policy.content_full).max(0.0);
                vec![((a, b), len - extra + extra / policy.content_speed)]
            }
            Kind::Busy => {
                let pace = policy.busy_target.max(len / policy.busy_max_speed);
                vec![((a, b), len.min(pace.max(hold)))]
            }
            Kind::Settled if len <= hold + policy.preroll => vec![((a, b), len)],
            Kind::Settled => vec![((a, a + hold), hold), ((b - policy.preroll, b), policy.preroll)],
            Kind::End => {
                let keep = len.min(hold.max(policy.end_min));
                vec![((a, a + keep), keep)]
            }
        };
        let start = out;
        for (take, len) in spans.into_iter().filter(|(_, len)| *len > 0.0) {
            pieces.push(Piece { out, len, take });
            out += len;
        }
        let quiet = matches!(seg.kind, Kind::Busy | Kind::Settled | Kind::End);
        edits.push(Edit {
            kind: seg.kind,
            take: [round(a), round(b)],
            out: [round(start), round(out)],
            words: quiet.then_some(seg.words),
            hold: matches!(seg.kind, Kind::Settled | Kind::End).then_some(round(hold)),
        });
    }
    let map = TimeMap {
        pieces,
        end: duration,
    };
    let report = Tightened {
        duration_raw: round(duration),
        duration: round(map.duration()),
        segments: edits,
    };
    (map, report)
}

fn round(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(t: f64) -> Point {
        Point::Input { t, chars: 0 }
    }

    fn change(t: f64, kind: Change, text: &str) -> Point {
        Point::Change {
            t,
            kind,
            text: text.into(),
        }
    }

    fn content(t: f64, text: &str) -> Point {
        change(t, Change::Content, text)
    }

    /// A take shaped like an agent driving a TUI: an opening screen, slow typed commands,
    /// a long spinner the app runs on its own, a long read, and a long idle tail.
    fn agent_take() -> (Vec<Point>, f64) {
        let mut points = vec![
            content(0.8, "Welcome to the app"),
            content(2.5, "Welcome to the app tips: press enter"),
            Point::Input { t: 13.2, chars: 4 },
            content(13.3, "help"),
            input(15.2),
            content(15.25, "Do you want to continue? yes no"),
            input(33.5),
            content(33.55, "Scanning your files"),
        ];
        for i in 0..100 {
            let t = 34.0 + i as f64 * 0.1;
            points.push(change(t, Change::Minor, "Scanning your files ⠋"));
        }
        let proposal: Vec<String> = (0..80).map(|i| format!("word{i}")).collect();
        points.push(content(44.5, &proposal.join(" ")));
        points.push(input(90.0));
        points.push(content(90.1, "Saved"));
        (points, 120.0)
    }

    fn tightened(points: &[Point], duration: f64) -> (Vec<Segment>, TimeMap, Tightened) {
        let segs = segment(points, duration);
        let (map, report) = plan(&segs, &POLICY, duration);
        (segs, map, report)
    }

    #[test]
    fn segments_cover_the_take_in_order() {
        let (points, duration) = agent_take();
        let segs = segment(&points, duration);
        assert_eq!(segs.first().unwrap().take.0, 0.0);
        assert_eq!(segs.last().unwrap().take.1, duration);
        for w in segs.windows(2) {
            assert_eq!(w[0].take.1, w[1].take.0, "{:?} then {:?}", w[0], w[1]);
        }
        let kinds: Vec<Kind> = segs.iter().map(|s| s.kind).collect();
        use Kind::*;
        assert_eq!(
            kinds,
            [
                Lead, Content, Busy, Content, Settled, Typing, Settled, Typing, Settled, Typing,
                Busy, Content, Settled, Typing, End
            ]
        );
    }

    #[test]
    fn time_map_is_monotonic_and_ends_on_the_take_end() {
        let (points, duration) = agent_take();
        let (_, map, report) = tightened(&points, duration);
        let frames = (map.duration() * FPS as f64).round() as usize;
        let mut last = 0.0;
        for f in 0..=frames {
            let t = map.take_time(f as f64 / FPS as f64);
            assert!(t >= last, "frame {f}: {t} < {last}");
            assert!((0.0..=duration).contains(&t));
            last = t;
        }
        assert!(map.duration() < duration / 2.0, "{}", map.duration());
        let summed: f64 = report.segments.iter().map(|e| e.out[1] - e.out[0]).sum();
        assert!((summed - map.duration()).abs() < 0.01);
        assert_eq!(report.segments.last().unwrap().out[1], round(map.duration()));
    }

    #[test]
    fn busy_compresses_to_its_target_and_no_faster_than_the_cap() {
        let (points, duration) = agent_take();
        let (segs, _, report) = tightened(&points, duration);
        let (i, spinner) = segs
            .iter()
            .enumerate()
            .find(|(_, s)| s.kind == Kind::Busy && s.len() > 5.0)
            .expect("the spinner is busy");
        let out = report.segments[i].out[1] - report.segments[i].out[0];
        let hold = POLICY.reading.hold(spinner.words);
        assert!(out <= POLICY.busy_target.max(hold) + 1e-9, "{out}");
        assert!(spinner.len() / out <= POLICY.busy_max_speed + 1e-9);

        let long = [content(1.0, "a"), content(200.0, "b")];
        let (segs, _, report) = tightened(&long, 201.0);
        let i = segs.iter().position(|s| s.kind == Kind::Busy).unwrap();
        let out = report.segments[i].out[1] - report.segments[i].out[0];
        assert!((out - 199.0 / POLICY.busy_max_speed).abs() < 0.01, "{out}");
    }

    #[test]
    fn settled_holds_for_reading_time_then_cuts_to_the_preroll() {
        let (points, duration) = agent_take();
        let (segs, map, report) = tightened(&points, duration);
        for (seg, edit) in segs.iter().zip(&report.segments) {
            if seg.kind != Kind::Settled {
                continue;
            }
            let out = edit.out[1] - edit.out[0];
            let hold = edit.hold.unwrap();
            assert!((POLICY.reading.min..=POLICY.reading.max).contains(&hold));
            assert!(out <= hold + POLICY.preroll + 0.002, "{edit:?}");
            assert!(out >= seg.len().min(hold + POLICY.preroll) - 0.002, "{edit:?}");
            // The last preroll of output plays the take right up to the input.
            let end = edit.out[1];
            let before = map.take_time(end - POLICY.preroll / 2.0);
            assert!(before > seg.take.1 - POLICY.preroll && before < seg.take.1);
        }
        let proposal = segs.iter().zip(&report.segments).find(|(s, _)| s.kind == Kind::Settled && s.take.0 == 44.5);
        assert_eq!(proposal.unwrap().1.hold, Some(POLICY.reading.max));
    }

    #[test]
    fn every_input_lands_in_played_time_after_its_preroll() {
        let (points, duration) = agent_take();
        let (_, map, _) = tightened(&points, duration);
        let played = |take: f64| {
            map.pieces
                .iter()
                .any(|p| p.take.0 <= take && take <= p.take.1)
        };
        for p in &points {
            if let Point::Input { t, .. } = p {
                assert!(played(*t), "input at {t} was cut");
                assert!(played(t - POLICY.preroll), "preroll before {t} was cut");
            }
        }
    }

    #[test]
    fn lead_is_cut_and_the_end_holds() {
        let (points, duration) = agent_take();
        let (segs, map, report) = tightened(&points, duration);
        assert_eq!(report.segments[0].out, [0.0, 0.0]);
        assert_eq!(map.take_time(0.0), 0.8);
        let end = report.segments.last().unwrap();
        assert_eq!(segs.last().unwrap().kind, Kind::End);
        assert!(end.out[1] - end.out[0] >= POLICY.end_min - 0.002);
        assert!(end.out[1] - end.out[0] <= POLICY.reading.max + 0.002);
    }

    #[test]
    fn content_plays_at_full_speed_up_to_the_cap() {
        let stream: Vec<Point> = (0..100)
            .map(|i| content(1.0 + i as f64 * 0.1, &format!("line {i}")))
            .collect();
        let (segs, _, report) = tightened(&stream, 20.0);
        let i = segs.iter().position(|s| s.kind == Kind::Content).unwrap();
        assert_eq!(segs[i].take, (1.0, 10.9));
        let out = report.segments[i].out[1] - report.segments[i].out[0];
        assert!((out - (3.0 + 6.9 / 2.0)).abs() < 0.01, "{out}");
    }

    #[test]
    fn identity_map_plays_the_take_as_recorded() {
        let map = TimeMap::identity(10.0);
        assert_eq!(map.duration(), 10.0);
        assert_eq!(map.take_time(2.5), 2.5);
        assert_eq!(map.take_time(11.0), 10.0);
    }

    #[test]
    fn new_words_counts_only_what_appeared() {
        assert_eq!(new_words("a b c", "a b c d d"), 2);
        assert_eq!(new_words("", "hello — world"), 2);
        assert_eq!(new_words("x y", "y"), 0);
    }

    fn screen(rows: &[&str]) -> vt100::Parser {
        let mut p = vt100::Parser::new(5, 40, 0);
        p.process(rows.join("\r\n").as_bytes());
        p
    }

    #[test]
    fn spinner_ticks_are_minor_and_new_lines_are_content() {
        let before = screen(&["$ build", "⠋ Working (1s)"]);
        let tick = screen(&["$ build", "⠙ Working (2s)"]);
        let line = screen(&["$ build", "⠋ Working (1s)", "ok"]);
        assert_eq!(
            classify(before.screen(), tick.screen()),
            Some(Change::Minor)
        );
        assert_eq!(
            classify(before.screen(), line.screen()),
            Some(Change::Content)
        );
        assert_eq!(classify(before.screen(), before.screen()), None);
    }
}
