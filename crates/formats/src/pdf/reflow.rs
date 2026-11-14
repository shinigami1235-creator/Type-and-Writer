//! Positioned glyphs, pictures and rules back into a flow: lines from baselines, columns from
//! the gutters between them, ruled tables from their grid, paragraphs from line spacing and
//! indents, headings from size and weight, lists from their markers. Page headers and footers
//! that repeat on most pages and bare page numbers are left out.

use std::collections::HashMap;
use std::sync::Arc;

use wordcraft_doc::{Align, Rgb};

use super::extract::{G, PImg, PageData, Rule};
use crate::model::{Cell, FBlock, FTable, Fmt, Inline, Kind, ListInfo, Para, make_img};

/// A glyph in a line, with its sub/superscript placement.
#[derive(Clone, Debug)]
struct LG {
    g: G,
    /// 1: superscript, -1: subscript.
    shift: i8,
}

/// One line of text (a run of glyphs on a baseline within one column).
#[derive(Clone, Debug)]
struct Line {
    glyphs: Vec<LG>,
    x0: f32,
    x1: f32,
    base: f32,
    /// Its main font size, points.
    size: f32,
    page: usize,
}

impl Line {
    fn new(mut glyphs: Vec<LG>, page: usize) -> Option<Line> {
        glyphs.sort_by(|a, b| a.g.x0.total_cmp(&b.g.x0));
        let main: Vec<&LG> = glyphs.iter().filter(|l| l.shift == 0).collect();
        let pick = if main.is_empty() { glyphs.iter().collect::<Vec<_>>() } else { main };
        let x0 = glyphs.iter().map(|l| l.g.x0).fold(f32::INFINITY, f32::min);
        let x1 = glyphs.iter().map(|l| l.g.x1).fold(f32::NEG_INFINITY, f32::max);
        let size = mode_size(pick.iter().map(|l| (l.g.size, l.g.text.chars().count())))?;
        let base = median(pick.iter().map(|l| l.g.base).collect())?;
        Some(Line { glyphs, x0, x1, base, size, page })
    }
    fn top(&self) -> f32 {
        self.base - self.size * 0.8
    }
    fn text(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.glyphs.iter().enumerate() {
            if i > 0 && gap_is_space(&self.glyphs[i - 1].g, &l.g) {
                s.push(' ');
            }
            s.push_str(&l.g.text);
        }
        s
    }
    fn all_bold(&self) -> bool {
        self.glyphs.iter().all(|l| l.g.bold)
    }
}

fn gap_is_space(a: &G, b: &G) -> bool {
    a.space_after || b.x0 - a.x1 > 0.18 * a.size.min(b.size)
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied()
}

/// The size most text is set in, weighted by characters, to the half point.
fn mode_size(it: impl Iterator<Item = (f32, usize)>) -> Option<f32> {
    let mut counts: HashMap<i32, usize> = HashMap::new();
    for (s, n) in it {
        *counts.entry((s * 2.0).round() as i32).or_default() += n.max(1);
    }
    counts.into_iter().max_by_key(|&(k, n)| (n, -k)).map(|(k, _)| k as f32 / 2.0)
}

/// Group glyphs into lines by baseline, then fold small raised or lowered glyphs into the
/// line beside them as super/subscripts.
fn lines_of(glyphs: Vec<G>, page: usize) -> Vec<Line> {
    let mut gs: Vec<G> = glyphs.into_iter().filter(|g| g.upright && !g.is_space()).collect();
    gs.sort_by(|a, b| a.base.total_cmp(&b.base).then(a.x0.total_cmp(&b.x0)));
    let mut clusters: Vec<(f32, f32, Vec<G>)> = Vec::new();
    for g in gs {
        match clusters.last_mut() {
            Some((base, size, v)) if (g.base - *base).abs() <= 0.3 * size.min(g.size).max(1.0) => v.push(g),
            _ => clusters.push((g.base, g.size, vec![g])),
        }
    }
    // Split each baseline cluster where text is far apart on the line: different columns or
    // table cells share a baseline but not a line.
    let mut lines: Vec<Line> = Vec::new();
    for (_, _, mut v) in clusters {
        v.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut cur: Vec<LG> = Vec::new();
        for g in v {
            // OCR words sit about a third of an em apart; a column gap is wider than an em.
            let split = if g.ocr { 1.1 } else { 1.8 };
            if let Some(last) = cur.last()
                && g.x0 - last.g.x1 > split * last.g.size.max(g.size)
            {
                lines.extend(Line::new(std::mem::take(&mut cur), page));
            }
            cur.push(LG { g, shift: 0 });
        }
        lines.extend(Line::new(cur, page));
    }
    // Superscripts and subscripts: a short line of small glyphs within half a line of a bigger
    // line it sits over.
    let mut i = 0;
    while i < lines.len() {
        let small = lines.get(i).is_some_and(|l| l.glyphs.len() <= 12);
        let mut merged = false;
        if small {
            let (sx0, sx1, sbase, ssize) = {
                let l = &lines[i];
                (l.x0, l.x1, l.base, l.size)
            };
            let target = lines.iter().enumerate().position(|(j, m)| {
                j != i && ssize <= 0.82 * m.size && (sbase - m.base).abs() <= 0.6 * m.size && sx0 >= m.x0 - m.size && sx1 <= m.x1 + 3.0 * m.size
            });
            if let Some(j) = target {
                let l = lines.remove(i);
                let j = if j > i { j - 1 } else { j };
                if let Some(m) = lines.get_mut(j) {
                    let shift = if l.base < m.base { 1 } else { -1 };
                    m.glyphs.extend(l.glyphs.into_iter().map(|mut g| {
                        g.shift = shift;
                        g
                    }));
                    if let Some(n) = Line::new(std::mem::take(&mut m.glyphs), page) {
                        *m = n;
                    }
                }
                merged = true;
            }
        }
        if !merged {
            i += 1;
        }
    }
    lines.sort_by(|a, b| a.base.total_cmp(&b.base).then(a.x0.total_cmp(&b.x0)));
    lines
}

/// Text with digits taken out, to match running headers and footers that number the page.
fn running_key(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_digit() && !c.is_whitespace()).collect::<String>().to_lowercase()
}

fn is_page_number(s: &str) -> bool {
    let t = s.trim().trim_matches(|c: char| c == '-' || c == '–' || c == '—' || c.is_whitespace());
    let t = t.strip_prefix("Page ").or_else(|| t.strip_prefix("page ")).unwrap_or(t);
    let core = t.split(" of ").next().unwrap_or(t).trim();
    (!core.is_empty() && core.len() <= 5 && core.chars().all(|c| c.is_ascii_digit()))
        || (!core.is_empty() && core.len() <= 6 && core.chars().all(|c| "ivxlcdm".contains(c.to_ascii_lowercase())))
}

/// Ruled tables found on a page: the box each covers, its row and column lines, and which grid
/// cells are merged.
#[derive(Debug)]
struct GridTable {
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// For each row, each column: (owner row, owner col) of the merged cell covering it.
    owner: Vec<Vec<(usize, usize)>>,
}

impl GridTable {
    fn rect(&self) -> (f32, f32, f32, f32) {
        (
            self.xs.first().copied().unwrap_or(0.0),
            self.ys.first().copied().unwrap_or(0.0),
            self.xs.last().copied().unwrap_or(0.0),
            self.ys.last().copied().unwrap_or(0.0),
        )
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        let (x0, y0, x1, y1) = self.rect();
        x >= x0 - 1.0 && x <= x1 + 1.0 && y >= y0 - 1.0 && y <= y1 + 1.0
    }
    fn cell_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let c = self.xs.windows(2).position(|w| x >= w[0] - 1.0 && x <= w[1] + 1.0)?;
        let r = self.ys.windows(2).position(|w| y >= w[0] - 1.0 && y <= w[1] + 1.0)?;
        self.owner.get(r)?.get(c).copied()
    }
}

/// Positions within `tol` of each other, as one (their mean), sorted.
fn cluster_positions(mut v: Vec<f32>, tol: f32) -> Vec<f32> {
    v.sort_by(f32::total_cmp);
    let mut out: Vec<(f32, usize)> = Vec::new();
    for x in v {
        match out.last_mut() {
            Some((m, n)) if (x - *m / *n as f32).abs() <= tol => {
                *m += x;
                *n += 1;
            }
            _ => out.push((x, 1)),
        }
    }
    out.into_iter().map(|(m, n)| m / n as f32).collect()
}

fn find_tables(rules: &[Rule]) -> Vec<GridTable> {
    const TOL: f32 = 2.0;
    let rules: Vec<Rule> = rules.iter().copied().take(4000).collect();
    let n = rules.len();
    if n < 4 {
        return Vec::new();
    }
    // Union-find over rules that touch (a horizontal crossing a vertical).
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        let mut guard = 0;
        while p.get(i).copied() != Some(i) && guard < 10_000 {
            let next = p.get(i).copied().unwrap_or(i);
            if let Some(gp) = p.get(next).copied()
                && let Some(slot) = p.get_mut(i)
            {
                *slot = gp;
            }
            i = next;
            guard += 1;
        }
        i
    }
    let touches = |h: &Rule, v: &Rule| v.x0 >= h.x0 - TOL && v.x0 <= h.x1 + TOL && h.y0 >= v.y0 - TOL && h.y0 <= v.y1 + TOL;
    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (rules[i], rules[j]);
            let t = match (a.horizontal(), b.horizontal()) {
                (true, false) => touches(&a, &b),
                (false, true) => touches(&b, &a),
                _ => false,
            };
            if t {
                let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                if ra != rb
                    && let Some(slot) = parent.get_mut(ra)
                {
                    *slot = rb;
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<Rule>> = HashMap::new();
    for (i, rule) in rules.iter().enumerate() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(*rule);
    }
    let mut out = Vec::new();
    for (_, g) in groups {
        let hs: Vec<&Rule> = g.iter().filter(|r| r.horizontal()).collect();
        let vs: Vec<&Rule> = g.iter().filter(|r| !r.horizontal()).collect();
        if hs.len() < 2 || vs.len() < 2 {
            continue;
        }
        let ys = cluster_positions(hs.iter().map(|r| r.y0).collect(), TOL);
        let xs = cluster_positions(vs.iter().map(|r| r.x0).collect(), TOL);
        if ys.len() < 2 || xs.len() < 2 || ys.len() > 200 || xs.len() > 63 {
            continue;
        }
        let (nr, nc) = (ys.len() - 1, xs.len() - 1);
        if nr * nc < 2 {
            continue; // one box around text is a frame, not a table
        }
        let (x0, x1) = (xs[0], xs[nc]);
        let (y0, y1) = (ys[0], ys[nr]);
        if x1 - x0 < 30.0 || y1 - y0 < 10.0 {
            continue;
        }
        let v_at = |x: f32, y: f32| vs.iter().any(|r| (r.x0 - x).abs() <= TOL && y >= r.y0 - TOL && y <= r.y1 + TOL);
        let h_at = |x: f32, y: f32| hs.iter().any(|r| (r.y0 - y).abs() <= TOL && x >= r.x0 - TOL && x <= r.x1 + TOL);
        let mut owner = vec![vec![(0usize, 0usize); nc]; nr];
        for r in 0..nr {
            let ym = (ys[r] + ys[r + 1]) / 2.0;
            for c in 0..nc {
                let xm = (xs[c] + xs[c + 1]) / 2.0;
                let left_open = c > 0 && !v_at(xs[c], ym);
                let top_open = r > 0 && !h_at(xm, ys[r]);
                owner[r][c] = if left_open {
                    owner[r][c - 1]
                } else if top_open {
                    owner[r - 1][c]
                } else {
                    (r, c)
                };
            }
        }
        out.push(GridTable { xs, ys, owner });
    }
    out.sort_by(|a, b| a.rect().1.total_cmp(&b.rect().1));
    out
}

/// A piece of a page in reading order.
enum Atom {
    Line(Line),
    Table(FTable),
    Image(PImg),
}

/// The x ranges of the gutters between columns of text on a page, left to right.
fn gutters(all: &[Line], w: f32) -> Vec<(f32, f32)> {
    // Lines across the whole text width (titles, full-width figures' captions) cross any
    // gutter; the columns show in the narrower lines.
    let left = all.iter().map(|l| l.x0).fold(f32::INFINITY, f32::min);
    let right = all.iter().map(|l| l.x1).fold(f32::NEG_INFINITY, f32::max);
    let span = (right - left).max(1.0);
    let narrow: Vec<Line> = all.iter().filter(|l| l.x1 - l.x0 < 0.6 * span).cloned().collect();
    let lines = &narrow[..];
    if lines.len() < 6 || w <= 0.0 || narrow.len() * 2 < all.len() {
        return Vec::new();
    }
    let cells = (w.ceil() as usize).min(5000);
    let mut cov = vec![0u32; cells + 1];
    for l in lines {
        let a = l.x0.max(0.0) as usize;
        let b = (l.x1.max(0.0) as usize).min(cells);
        for c in cov.iter_mut().take(b + 1).skip(a) {
            *c += 1;
        }
    }
    // Centred lines (names, captions) may cross a gutter; columns of text mostly don't.
    let allowed = (lines.len() as f32 * 0.15).floor() as u32;
    let left_text = lines.iter().map(|l| l.x0).fold(f32::INFINITY, f32::min).max(0.0) as usize;
    let right_text = lines.iter().map(|l| l.x1).fold(0.0, f32::max) as usize;
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for x in left_text..=right_text.min(cells) {
        let free = cov.get(x).is_some_and(|&c| c <= allowed);
        match (free, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                let (g0, g1) = (s as f32, x as f32);
                let lw: Vec<f32> = lines.iter().filter(|l| l.x1 <= g0 + 1.0).map(|l| l.x1 - l.x0).collect();
                let rw: Vec<f32> = lines.iter().filter(|l| l.x0 >= g1 - 1.0).map(|l| l.x1 - l.x0).collect();
                let (left, right) = (lw.len(), rw.len());
                let n = lines.len();
                // Columns are blocks of text, not a ragged list of labels and values.
                let wide = |v: Vec<f32>| !v.is_empty() && v.iter().filter(|&&w| w >= 0.3 * span).count() * 3 >= v.len();
                if g1 - g0 >= 7.0 && left * 4 >= n && right * 4 >= n && wide(lw) && wide(rw) {
                    out.push((g0, g1));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Paragraph-level reading of the whole document.
pub struct Reflow {
    pub blocks: Vec<FBlock>,
    /// Smallest left/right/top/bottom distance of text from the page edges, points.
    pub margins: [f32; 4],
}

/// How far apart this document sets lines of each size (baseline to baseline), points.
fn line_pitches(pages: &[Vec<Line>]) -> HashMap<i32, f32> {
    let mut by: HashMap<i32, Vec<f32>> = HashMap::new();
    for lines in pages {
        let mut sorted: Vec<&Line> = lines.iter().collect();
        sorted.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.base.total_cmp(&b.base)));
        for w in lines.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let d = b.base - a.base;
            if (a.size - b.size).abs() < 0.3 && (a.x0 - b.x0).abs() < 4.0 * a.size && d > 0.8 * a.size && d < 2.5 * a.size {
                by.entry((a.size * 2.0).round() as i32).or_default().push(d);
            }
        }
    }
    by.into_iter().filter_map(|(k, v)| median(v).map(|m| (k, m))).collect()
}

fn pitch_for(p: &HashMap<i32, f32>, size: f32) -> f32 {
    p.get(&((size * 2.0).round() as i32)).copied().unwrap_or(size * 1.2)
}

/// A list marker at the start of `text`: (ordered, start number, marker length in bytes).
fn list_marker(text: &str) -> Option<(bool, u32, usize)> {
    let t = text.trim_start();
    let lead = text.len() - t.len();
    let mut chars = t.char_indices();
    let (_, c) = chars.next()?;
    if "•●○◦▪■□–‣⁃∙·*-➢►▶✓✔".contains(c) {
        let rest = &t[c.len_utf8()..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            return Some((false, 1, lead + c.len_utf8() + 1));
        }
        return None;
    }
    // 1. 1) (1) a. a) i.
    let (open, body_start) = if c == '(' { (true, 1) } else { (false, 0) };
    let body = &t[body_start..];
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    let (num, used) = if !digits.is_empty() && digits.len() <= 3 {
        (digits.parse::<u32>().ok()?, digits.len())
    } else {
        let l = body.chars().next()?;
        if l.is_ascii_lowercase() && body.chars().nth(1).is_some_and(|c| c == '.' || c == ')') {
            ((l as u32).saturating_sub('a' as u32) + 1, 1)
        } else {
            return None;
        }
    };
    let after = &body[used..];
    let close = after.chars().next()?;
    let ok = if open { close == ')' } else { close == '.' || close == ')' };
    if !ok || !after[1..].starts_with([' ', '\t']) {
        return None;
    }
    Some((true, num, lead + body_start + used + 2))
}

fn fmt_of(g: &G, shift: i8, _body: f32) -> Fmt {
    let size = (g.size * 2.0).round() / 2.0;
    let black = g.color == Rgb(0, 0, 0);
    Fmt {
        bold: g.bold,
        italic: g.italic,
        code: false,
        sup: shift > 0,
        sub: shift < 0,
        color: (!black).then_some(g.color),
        size: (shift == 0).then_some(size),
        font: g.family.clone(),
        ..Default::default()
    }
}

struct Builder<'a> {
    pitches: &'a HashMap<i32, f32>,
    body: f32,
    blocks: Vec<FBlock>,
    /// The paragraph being built: its lines.
    cur: Vec<Line>,
    /// Column bounds of the current paragraph's column.
    col: (f32, f32),
    first_heading_done: bool,
}

impl Builder<'_> {
    fn flush(&mut self) {
        let lines = std::mem::take(&mut self.cur);
        if let Some(p) = self.para(&lines) {
            self.blocks.push(FBlock::Para(p));
        }
    }

    fn short(&self, l: &Line) -> bool {
        let (c0, c1) = self.col;
        l.x1 < c1 - (2.0 * l.size).max(0.1 * (c1 - c0))
    }

    /// Does line `l` start a new paragraph after `prev`?
    fn breaks(&self, prev: &Line, l: &Line, same_flow: bool) -> bool {
        let ends_sentence = prev.text().trim_end().ends_with(['.', '!', '?', ':', '"', '”']);
        if (prev.size - l.size).abs() > 0.75 {
            return true;
        }
        if list_marker(&l.text()).is_some() {
            return true;
        }
        let (c0, _) = self.col;
        let indented = l.x0 > c0 + 0.6 * l.size;
        if !same_flow {
            // A column or page turn: the paragraph carries on when the last line ran full width
            // and didn't end a sentence.
            return self.short(prev) || ends_sentence || indented;
        }
        let gap = l.base - prev.base;
        let pitch = pitch_for(self.pitches, prev.size);
        if gap > pitch * 1.35 + 0.5 || gap < 0.0 {
            return true;
        }
        if prev.all_bold() != l.all_bold() && (self.short(prev) || prev.all_bold()) {
            return true;
        }
        if self.short(prev) && (ends_sentence || indented || prev.all_bold()) {
            return true;
        }
        let prev_indented = prev.x0 > c0 + 0.6 * prev.size;
        let in_list = self.cur.first().is_some_and(|f| list_marker(&f.text()).is_some());
        indented && !prev_indented && !self.short(prev) && !in_list
    }

    fn push_line(&mut self, l: Line, col: (f32, f32), same_flow: bool) {
        let brk = match self.cur.last() {
            Some(prev) => self.breaks(prev, &l, same_flow),
            None => false,
        };
        if brk {
            self.flush();
        }
        if self.cur.is_empty() {
            self.col = col;
        }
        self.cur.push(l);
    }

    /// Title or heading level from size and weight against the body text.
    fn kind_of(&self, lines: &[Line], size: f32) -> Option<Kind> {
        let first = lines.first()?;
        let all_bold = lines.iter().all(Line::all_bold);
        let text_len: usize = lines.iter().map(|l| l.glyphs.len()).sum();
        let ratio = size / self.body.max(1.0);
        let one_liner = lines.len() <= 2 && text_len <= 120;
        let last_text = lines.last().map(Line::text).unwrap_or_default();
        let bold_heading = one_liner
            && all_bold
            && lines.len() == 1
            && ratio >= 0.95
            && text_len >= 4
            && first.x0 <= self.col.0 + 0.5 * first.size
            && !last_text.trim_end().ends_with(['.', ',', ';']);
        Some(if one_liner && ratio >= 1.9 && !self.first_heading_done && first.page == 0 {
            Kind::Title
        } else if one_liner && ratio >= 1.45 {
            Kind::Heading(1)
        } else if one_liner && ratio >= 1.18 {
            Kind::Heading(2)
        } else if bold_heading {
            Kind::Heading(3)
        } else {
            Kind::Normal
        })
    }

    /// Alignment from where the lines sit in the column: (alignment, centred, right).
    fn align_of(&self, lines: &[Line]) -> (Option<Align>, bool, bool) {
        let Some(first) = lines.first() else { return (None, false, false) };
        let (c0, c1) = self.col;
        let mid = (c0 + c1) / 2.0;
        let centred = lines.iter().all(|l| ((l.x0 + l.x1) / 2.0 - mid).abs() < 4.0) && first.x0 > c0 + 2.0 * first.size;
        let right = lines.iter().all(|l| (l.x1 - c1).abs() < 3.0) && lines.iter().any(|l| l.x0 > c0 + 3.0 * l.size);
        let full = lines.len() >= 3 && lines.iter().take(lines.len() - 1).all(|l| (l.x1 - c1).abs() < 2.5);
        let align = if centred {
            Some(Align::Center)
        } else if right {
            Some(Align::Right)
        } else if full {
            Some(Align::Justify)
        } else {
            None
        };
        (align, centred, right)
    }

    fn para(&mut self, lines: &[Line]) -> Option<Para> {
        let first = lines.first()?;
        let mut p = Para::default();
        let size = mode_size(lines.iter().flat_map(|l| l.glyphs.iter().map(|g| (g.g.size, g.g.text.chars().count()))))?;
        p.kind = self.kind_of(lines, size)?;
        if matches!(p.kind, Kind::Title | Kind::Heading(_)) {
            self.first_heading_done = true;
        }
        // List item?
        let mut skip_chars = 0usize;
        if matches!(p.kind, Kind::Normal)
            && let Some((ordered, start, len)) = list_marker(&first.text())
        {
            let (c0, _) = self.col;
            let level = ((first.x0 - c0) / 18.0).floor().clamp(0.0, 8.0) as u8;
            p.list = Some(ListInfo { ordered, level, start, ..Default::default() });
            let t = first.text();
            skip_chars = t.get(..len).unwrap_or("").chars().filter(|c| !c.is_whitespace()).count();
        }
        let (align, centred, right) = self.align_of(lines);
        p.align = if matches!(align, Some(Align::Justify)) && p.list.is_some() { None } else { align };
        // First-line indent.
        if p.list.is_none() && !centred && !right && lines.len() >= 2 {
            let rest = lines.get(1).map_or(first.x0, |l| l.x0);
            if first.x0 > rest + 0.6 * first.size {
                p.spacing.first = Some((first.x0 - rest).min(72.0));
            }
        }
        // Text: glyph by glyph, lines joined with a space, a hyphen at a line end joined away.
        let mut skip = skip_chars;
        for (li, l) in lines.iter().enumerate() {
            if li > 0 && !p.inlines.is_empty() {
                let ends_hyphen = p.inlines.last().is_some_and(|i| matches!(i, Inline::Text(t, _) if t.ends_with('-') && !t.ends_with(" -")));
                let next_lower = l.glyphs.first().is_some_and(|g| g.g.text.chars().next().is_some_and(char::is_lowercase));
                if ends_hyphen && next_lower {
                    if let Some(Inline::Text(t, _)) = p.inlines.last_mut() {
                        t.pop();
                    }
                } else if let Some(prev) = l.glyphs.first() {
                    p.push_text(" ", &fmt_of(&prev.g, 0, self.body));
                }
            }
            for (gi, lg) in l.glyphs.iter().enumerate() {
                let mut text: &str = lg.g.text.as_str();
                if skip > 0 {
                    let n = text.chars().count();
                    if n <= skip {
                        skip -= n;
                        continue;
                    }
                    let cut = text.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(text.len());
                    skip = 0;
                    text = text.get(cut..).unwrap_or("");
                }
                if gi > 0 && !p.inlines.is_empty() {
                    let prev = &l.glyphs[gi - 1];
                    if lg.g.x0 - prev.g.x1 > 1.8 * prev.g.size.max(lg.g.size) {
                        p.push_text("\t", &fmt_of(&prev.g, 0, self.body));
                    } else if gap_is_space(&prev.g, &lg.g) {
                        p.push_text(" ", &fmt_of(&prev.g, prev.shift, self.body));
                    }
                }
                p.push_text(text, &fmt_of(&lg.g, lg.shift, self.body));
            }
        }
        if p.inlines.is_empty() {
            return None;
        }
        // Headings take their colour from the file, not from the heading style.
        if matches!(p.kind, Kind::Title | Kind::Heading(_)) {
            for i in &mut p.inlines {
                if let Inline::Text(_, f) = i
                    && f.color.is_none()
                {
                    f.color = Some(Rgb(0, 0, 0));
                }
            }
        }
        p.spacing.before = Some(0.0);
        Some(p)
    }
}

impl Builder<'_> {
    /// One page's lines, tables and pictures in reading order: top to bottom, and within a band
    /// between full-width pieces, column by column.
    fn place_page(
        &mut self,
        pi: usize,
        w: f32,
        lines: Vec<Line>,
        tables: Vec<(GridTable, Vec<G>)>,
        images: Vec<PImg>,
        prev_flow: &mut Option<(usize, usize)>,
    ) {
        let gut = gutters(&lines, w);
        let ncol = gut.len() + 1;
        let col_of = |x0: f32, x1: f32| -> Option<usize> {
            // Text crossing a gutter spans the columns.
            if gut.iter().any(|&(g0, g1)| x0 < g0 - 1.0 && x1 > g1 + 1.0) {
                return None;
            }
            let c = (x0 + x1) / 2.0;
            Some(gut.iter().filter(|&&(g0, _)| c > g0).count())
        };
        let col_or_one = |x0: f32, x1: f32| if ncol == 1 { Some(0) } else { col_of(x0, x1) };
        // Column bounds, from the lines in each; the last entry is the full width.
        let mut bounds = vec![(f32::INFINITY, f32::NEG_INFINITY); ncol + 1];
        for l in &lines {
            let k = col_of(l.x0, l.x1).unwrap_or(ncol);
            if let Some(bd) = bounds.get_mut(k) {
                bd.0 = bd.0.min(l.x0);
                bd.1 = bd.1.max(l.x1);
            }
        }
        let x0 = bounds.iter().map(|b| b.0).fold(f32::INFINITY, f32::min);
        let x1 = bounds.iter().map(|b| b.1).fold(f32::NEG_INFINITY, f32::max);
        let full_bounds = if x0.is_finite() && x1.is_finite() { (x0, x1) } else { (72.0, w - 72.0) };
        for bd in &mut bounds {
            if !bd.0.is_finite() || !bd.1.is_finite() {
                *bd = full_bounds;
            }
        }
        let mut atoms: Vec<(Option<usize>, f32, Atom)> = Vec::new();
        for l in lines {
            atoms.push((col_or_one(l.x0, l.x1), l.top(), Atom::Line(l)));
        }
        for (t, gs) in tables {
            let (tx0, ty0, tx1, _) = t.rect();
            atoms.push((col_or_one(tx0, tx1), ty0, Atom::Table(table_of(&t, gs, pi, self.pitches, self.body))));
        }
        for i in images {
            let (c, top) = (col_or_one(i.rect.x0 as f32, i.rect.x1 as f32), i.rect.y0 as f32);
            atoms.push((c, top, Atom::Image(i)));
        }
        atoms.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut ordered: Vec<(Option<usize>, Atom)> = Vec::with_capacity(atoms.len());
        let mut band: Vec<Vec<Atom>> = (0..ncol).map(|_| Vec::new()).collect();
        for (c, _, a) in atoms {
            match c.and_then(|k| band.get_mut(k)) {
                Some(v) => v.push(a),
                None => {
                    for (k, v) in band.iter_mut().enumerate() {
                        ordered.extend(v.drain(..).map(|a| (Some(k), a)));
                    }
                    ordered.push((None, a));
                }
            }
        }
        for (k, v) in band.iter_mut().enumerate() {
            ordered.extend(v.drain(..).map(|a| (Some(k), a)));
        }
        for (c, a) in ordered {
            let col = c.and_then(|k| bounds.get(k).copied()).unwrap_or(full_bounds);
            self.place_atom(a, col, (pi, c.unwrap_or(usize::MAX)), prev_flow);
        }
    }

    fn place_atom(&mut self, a: Atom, col: (f32, f32), flow: (usize, usize), prev_flow: &mut Option<(usize, usize)>) {
        match a {
            Atom::Line(l) => {
                let same = *prev_flow == Some(flow);
                self.push_line(l, col, same);
                *prev_flow = Some(flow);
            }
            Atom::Table(t) => {
                self.flush();
                self.blocks.push(FBlock::Table(t));
                *prev_flow = None;
            }
            Atom::Image(i) => {
                self.flush();
                let (iw, ih) = (i.rect.width() as f32, i.rect.height() as f32);
                let max_w = (col.1 - col.0).max(72.0);
                let s = if iw > max_w { max_w / iw } else { 1.0 };
                if let Some(img) = make_img(Arc::clone(&i.png), Some(iw * s), Some(ih * s), "") {
                    let mut p = Para::default();
                    p.inlines.push(Inline::Image(img));
                    let centred = ((i.rect.x0 + i.rect.x1) as f32 / 2.0 - (col.0 + col.1) / 2.0).abs() < 10.0;
                    p.align = centred.then_some(Align::Center);
                    self.blocks.push(FBlock::Para(p));
                }
                *prev_flow = None;
            }
        }
    }
}

/// Paragraphs of the glyphs inside one table cell.
fn cell_blocks(glyphs: Vec<G>, page: usize, pitches: &HashMap<i32, f32>, body: f32, bounds: (f32, f32)) -> Vec<FBlock> {
    let lines = lines_of(glyphs, page);
    let mut b = Builder { pitches, body, blocks: Vec::new(), cur: Vec::new(), col: bounds, first_heading_done: true };
    for l in lines {
        b.push_line(l, bounds, true);
    }
    b.flush();
    for blk in &mut b.blocks {
        if let FBlock::Para(p) = blk {
            if matches!(p.kind, Kind::Heading(_) | Kind::Title) {
                p.kind = Kind::Normal;
            }
            p.spacing.after = Some(0.0);
        }
    }
    b.blocks
}

fn table_of(t: &GridTable, glyphs: Vec<G>, page: usize, pitches: &HashMap<i32, f32>, body: f32) -> FTable {
    let nr = t.ys.len().saturating_sub(1);
    let nc = t.xs.len().saturating_sub(1);
    let mut per: HashMap<(usize, usize), Vec<G>> = HashMap::new();
    for g in glyphs {
        let cx = (g.x0 + g.x1) / 2.0;
        let cy = g.base - 0.3 * g.size;
        if let Some(rc) = t.cell_at(cx, cy) {
            per.entry(rc).or_default().push(g);
        }
    }
    let mut rows = Vec::with_capacity(nr);
    for r in 0..nr {
        let mut row = Vec::new();
        let mut c = 0;
        while c < nc {
            let own = t.owner.get(r).and_then(|o| o.get(c)).copied().unwrap_or((r, c));
            // Columns this merged cell spans in this row.
            let mut span = 1;
            while c + span < nc && t.owner.get(r).and_then(|o| o.get(c + span)).copied() == Some(own) {
                span += 1;
            }
            let covered = own.0 != r;
            let rowspan = if covered {
                1
            } else {
                let mut k = 1;
                while r + k < nr && t.owner.get(r + k).and_then(|o| o.get(c)).copied() == Some(own) {
                    k += 1;
                }
                k
            };
            let bounds = (t.xs.get(c).copied().unwrap_or(0.0), t.xs.get(c + span).copied().unwrap_or(0.0));
            let blocks = if covered { Vec::new() } else { cell_blocks(per.remove(&own).unwrap_or_default(), page, pitches, body, bounds) };
            row.push(Cell { blocks, colspan: span as u32, rowspan: rowspan as u32, covered, ..Default::default() });
            c += span;
        }
        rows.push(row);
    }
    let widths = t.xs.windows(2).map(|w| (w[1] - w[0]).max(6.0)).collect();
    FTable { rows, widths, borderless: false, ..Default::default() }
}

/// Read the pages' positioned content back into blocks.
/// One page's glyphs split between its ruled tables and the running text, and the pictures
/// worth keeping (a page-size picture behind text is a scan's image or a background).
fn split_page(p: PageData, pi: usize) -> (Vec<Line>, Vec<(GridTable, Vec<G>)>, Vec<PImg>) {
    let tables = find_tables(&p.rules);
    let mut inside: Vec<Vec<G>> = (0..tables.len()).map(|_| Vec::new()).collect();
    let mut free = Vec::with_capacity(p.glyphs.len());
    for g in p.glyphs {
        let (cx, cy) = ((g.x0 + g.x1) / 2.0, g.base - 0.3 * g.size);
        match tables.iter().position(|t| t.contains(cx, cy)) {
            Some(i) => {
                if let Some(v) = inside.get_mut(i) {
                    v.push(g);
                }
            }
            None => free.push(g),
        }
    }
    // A grid with no text in it is a drawing, not a table.
    let kept: Vec<(GridTable, Vec<G>)> = tables.into_iter().zip(inside).filter(|(_, gs)| !gs.is_empty()).collect();
    let page_area = (p.w * p.h).max(1.0);
    let has_text = !free.is_empty();
    let images = p.images.into_iter().filter(|i| !(has_text && (i.rect.width() * i.rect.height()) as f32 > 0.6 * page_area)).collect();
    (lines_of(free, pi), kept, images)
}

/// Leave out running headers and footers (the same text, numbers aside, at the top or bottom
/// of most pages) and bare page numbers there.
fn drop_running_lines(page_lines: &mut [Vec<Line>], sizes: &[(f32, f32)]) {
    let n_pages = page_lines.len();
    let height = |pi: usize| sizes.get(pi).map(|s| s.1).unwrap_or(792.0);
    let edge = |l: &Line, h: f32| l.base < 0.1 * h || l.top() > 0.9 * h;
    let mut seen: HashMap<String, usize> = HashMap::new();
    if n_pages >= 2 {
        for (pi, lines) in page_lines.iter().enumerate() {
            let h = height(pi);
            let mut keys: Vec<String> = lines.iter().filter(|l| edge(l, h)).map(|l| running_key(&l.text())).filter(|k| !k.is_empty()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                *seen.entry(k).or_default() += 1;
            }
        }
    }
    let need = (n_pages / 2).max(2);
    for (pi, lines) in page_lines.iter_mut().enumerate() {
        let h = height(pi);
        lines.retain(|l| {
            if !edge(l, h) {
                return true;
            }
            let t = l.text();
            if is_page_number(&t) {
                return false;
            }
            let k = running_key(&t);
            k.is_empty() || seen.get(&k).copied().unwrap_or(0) < need
        });
    }
}

/// The smallest left, right, top and bottom distance of text from the page edges.
fn text_margins(page_lines: &[Vec<Line>], sizes: &[(f32, f32)]) -> [f32; 4] {
    let mut margins = [f32::INFINITY; 4];
    for (pi, lines) in page_lines.iter().enumerate() {
        let (w, h) = sizes.get(pi).copied().unwrap_or((612.0, 792.0));
        for l in lines {
            margins[0] = margins[0].min(l.x0);
            margins[1] = margins[1].min(w - l.x1);
            margins[2] = margins[2].min(l.top());
            margins[3] = margins[3].min(h - (l.base + 0.3 * l.size));
        }
    }
    margins
}

pub fn reflow(pages: Vec<PageData>) -> Reflow {
    let n_pages = pages.len();
    let mut page_lines: Vec<Vec<Line>> = Vec::with_capacity(n_pages);
    let mut page_tables: Vec<Vec<(GridTable, Vec<G>)>> = Vec::with_capacity(n_pages);
    let mut page_images: Vec<Vec<PImg>> = Vec::with_capacity(n_pages);
    let mut sizes: Vec<(f32, f32)> = Vec::with_capacity(n_pages);
    for (pi, p) in pages.into_iter().enumerate() {
        sizes.push((p.w, p.h));
        let (lines, tables, images) = split_page(p, pi);
        page_lines.push(lines);
        page_tables.push(tables);
        page_images.push(images);
    }
    drop_running_lines(&mut page_lines, &sizes);
    let pitches = line_pitches(&page_lines);
    let body = mode_size(page_lines.iter().flatten().flat_map(|l| l.glyphs.iter().map(|g| (g.g.size, g.g.text.chars().count())))).unwrap_or(11.0);
    let mut margins = text_margins(&page_lines, &sizes);
    let mut b = Builder { pitches: &pitches, body, blocks: Vec::new(), cur: Vec::new(), col: (0.0, 612.0), first_heading_done: false };
    let mut prev_flow: Option<(usize, usize)> = None;
    for (pi, ((lines, tables), images)) in page_lines.into_iter().zip(page_tables).zip(page_images).enumerate() {
        let w = sizes.get(pi).map_or(612.0, |s| s.0);
        b.place_page(pi, w, lines, tables, images, &mut prev_flow);
    }
    b.flush();
    for m in &mut margins {
        if !m.is_finite() {
            *m = 72.0;
        }
    }
    Reflow { blocks: b.blocks, margins }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_markers() {
        assert_eq!(list_marker("• first"), Some((false, 1, 4)));
        assert_eq!(list_marker("3. third"), Some((true, 3, 3)));
        assert_eq!(list_marker("(2) second"), Some((true, 2, 4)));
        assert_eq!(list_marker("b) bee"), Some((true, 2, 3)));
        assert_eq!(list_marker("3.5 percent"), None);
        assert_eq!(list_marker("Dr. Smith"), None);
        assert_eq!(list_marker("-5 degrees"), None);
    }

    #[test]
    fn page_numbers() {
        assert!(is_page_number("12"));
        assert!(is_page_number("- 4 -"));
        assert!(is_page_number("Page 3 of 10"));
        assert!(is_page_number("iv"));
        assert!(!is_page_number("Introduction"));
    }
}
