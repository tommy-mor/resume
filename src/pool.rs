//! Pool loading: named-header `.txt` files with strict validation.
//!
//! Every pool file is a sequence of fields, each introduced by a header
//! line of the form `--name--` (see `pool/README.md`). Headers may appear
//! in any order but must be unique, and the set of fields is fixed per
//! file kind. A bare `--` line, a duplicate header, an unknown field, a
//! missing required field, or content before the first header is a hard
//! error that names the file and line — there is no silent fallback and
//! no default padding. What you wrote is what renders, or the build
//! stops and tells you exactly where.
//!
//! `--dates--` is formalized (not free text): `YYYY[-MM]` or
//! `START - END` where `END` may be `now` (see [`parse_dates`]). The
//! value is parsed once into a [`DateRange`], and `Entry::dates` stores
//! the pretty rendering (`2024 - Aug 2026`). Experience is filtered by
//! salience then displayed newest-first by start date; projects keep
//! salience order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A parsed `--dates--` value: start and end month, each `None` for
/// year-only, plus whether the range is open-ended (`now`).
#[derive(Debug, Clone, Copy)]
pub struct DateRange {
    pub start_year: u32,
    pub start_month: Option<u8>,
    pub end_year: u32,
    pub end_month: Option<u8>,
    pub is_now: bool,
}

/// Sorting key: `(year, month)` with year-only starts treated as the
/// earliest in the year (0) and year-only `now`/ends handled by the
/// caller. Main comparator in `main.rs` sorts descending by
/// `(start, end)` with `now` ends at `(9999, 12)`.
impl DateRange {
    /// `(year, month)` start key; month `0` when year-only.
    pub fn start_key(&self) -> (u32, u8) {
        (self.start_year, self.start_month.unwrap_or(0))
    }

    /// `(year, month)` end key; month `12` when year-only (the full
    /// year), `(9999, 12)` when open-ended.
    pub fn end_key(&self) -> (u32, u8) {
        if self.is_now {
            (9999, 12)
        } else {
            (self.end_year, self.end_month.unwrap_or(12))
        }
    }
}

/// One parsed entry (experience / project / education). `file`/`kind` are
/// kept for error messages and debugging. `dates` is the pretty display
/// string; `range` is its machine form.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Entry {
    pub id: String,
    pub kind: &'static str,
    pub file: String,
    pub title: String,
    pub company: String,
    pub dates: String,
    pub range: DateRange,
    pub location: String,
    pub tags: Vec<String>,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Pool {
    pub name: String,
    pub phone: String,
    pub email: String,
    pub links: Vec<String>,
    pub education: Vec<Entry>,
    pub experience: Vec<Entry>,
    pub projects: Vec<Entry>,
    /// (category header, value) in file order; header is rendered title-cased.
    pub skills: Vec<(String, String)>,
}

/// A named field: header name (lowercase, no `--`), trimmed value, and the
/// 1-based line of its header (for error messages).
struct Field {
    name: String,
    value: String,
    line: usize,
}

/// True if `trimmed` is a named header `--name--` with a non-empty name of
/// lowercase letters, digits, hyphens, or underscores.
fn is_header(trimmed: &str) -> Option<String> {
    let inner = trimmed.strip_prefix("--")?.strip_suffix("--")?;
    if inner.is_empty()
        || !inner
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return None;
    }
    Some(inner.to_string())
}

/// Split file text into named fields. Errors name the file and line.
fn parse_fields(path: &Path, text: &str) -> Result<Vec<Field>, String> {
    let path_s = path.display();
    if text.trim().is_empty() {
        return Err(format!("{path_s}: file is empty"));
    }
    let mut fields: Vec<Field> = Vec::new();
    // `current` accumulates value lines for the open field.
    let mut current: Option<(String, usize, Vec<String>)> = None;
    let flush = |fields: &mut Vec<Field>, cur: Option<(String, usize, Vec<String>)>| {
        if let Some((name, line, lines)) = cur {
            let value = lines.join("\n").trim().to_string();
            fields.push(Field { name, value, line });
        }
    };
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let trimmed = raw.trim();
        if let Some(name) = is_header(trimmed) {
            flush(&mut fields, current.take());
            current = Some((name, lineno, Vec::new()));
        } else if trimmed == "--" || trimmed.starts_with("--") {
            // Almost a header but not a valid named one (bare `--`, uppercase,
            // spaces, `--title` without the closing dashes, ...). Say what the
            // shape should have been instead of guessing.
            return Err(format!(
                "{path_s}:{lineno}: `{trimmed}` is not a valid field header; \
                 headers look like `--title--` (lowercase name between two `--`)"
            ));
        } else if let Some((_, _, lines)) = current.as_mut() {
            lines.push(raw.to_string());
        } else if !trimmed.is_empty() {
            return Err(format!(
                "{path_s}:{lineno}: content before the first `--field--` header: \
                 `{trimmed}`"
            ));
        }
        // Blank lines before the first header are tolerated whitespace.
    }
    flush(&mut fields, current);
    if fields.is_empty() {
        return Err(format!("{path_s}: no `--field--` headers found"));
    }
    Ok(fields)
}

/// Index fields by name, rejecting duplicates and unknown names.
fn index_fields(
    path: &Path,
    fields: &[Field],
    allowed: &[&str],
) -> Result<HashMap<String, (String, usize)>, String> {
    let path_s = path.display();
    let mut map: HashMap<String, (String, usize)> = HashMap::new();
    for f in fields {
        if !allowed.contains(&f.name.as_str()) {
            return Err(format!(
                "{path_s}: unknown field `--{}--` at line {} (allowed: {})",
                f.name,
                f.line,
                allowed.join(", ")
            ));
        }
        if map.contains_key(&f.name) {
            return Err(format!(
                "{path_s}: duplicate field `--{}--` at line {} (first at line {})",
                f.name,
                f.line,
                map[&f.name].1
            ));
        }
        map.insert(f.name.clone(), (f.value.clone(), f.line));
    }
    Ok(map)
}

/// Value of a required field: must be present and non-empty.
fn required(path: &Path, map: &HashMap<String, (String, usize)>, name: &str) -> Result<String, String> {
    match map.get(name) {
        None => Err(format!("{}: missing required field `--{name}--`", path.display())),
        Some((v, _)) if v.trim().is_empty() => {
            Err(format!("{}: required field `--{name}--` is empty", path.display()))
        }
        Some((v, _)) => Ok(v.trim().to_string()),
    }
}

/// Value of an optional field: `""` when omitted or blank.
fn optional(map: &HashMap<String, (String, usize)>, name: &str) -> String {
    map.get(name).map(|(v, _)| v.trim().to_string()).unwrap_or_default()
}

/// Split a list value into one item per non-empty line, stripping a leading
/// `- `, `* `, or `• ` bullet marker when present.
fn parse_list(value: &str) -> Vec<String> {
    value
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).unwrap_or(l);
            let l = l.strip_prefix("• ").unwrap_or(l);
            l.trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Split a tags value on commas and/or newlines; lowercase, dedupe.
fn parse_tags(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in value.replace('\n', ",").split(',') {
        let t = part.trim().to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

const EXPECTED_DATES: &str = "`YYYY[-MM]` or `START - END` where END may be `now`, \
     e.g. `2024`, `2025-06`, `2024 - 2026-08`, `2023 - now`";

fn parse_ym(s: &str) -> Option<(u32, Option<u8>)> {
    let t = s.trim();
    if let Some((y, m)) = t.split_once('-') {
        let year: u32 = y.trim().parse().ok()?;
        let month: u8 = m.trim().parse().ok()?;
        if y.trim().len() == 4 && (1900..=2100).contains(&year) && (1..=12).contains(&month) {
            Some((year, Some(month)))
        } else {
            None
        }
    } else if t.len() == 4 {
        let year: u32 = t.parse().ok()?;
        if (1900..=2100).contains(&year) {
            Some((year, None))
        } else {
            None
        }
    } else {
        None
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn pretty_ym(year: u32, month: Option<u8>) -> String {
    match month {
        Some(m) => format!("{} {}", MONTHS[(m - 1) as usize], year),
        None => year.to_string(),
    }
}

/// Parse and validate a `--dates--` value. Returns the pretty display
/// string plus the [`DateRange`]. The file's dates line is included in
/// every error.
pub fn parse_dates(path: &Path, line: usize, raw: &str) -> Result<(String, DateRange), String> {
    let path_s = path.display();
    let err = |msg: &str| format!("{path_s}:{line}: invalid `--dates--` {raw:?}: {msg}; expected {EXPECTED_DATES}");
    let parts: Vec<&str> = raw.split(" - ").collect();
    if parts.len() > 2 {
        return Err(err("more than one ` - ` separator"));
    }
    let is_range = parts.len() == 2;
    let (sy, sm) = parse_ym(parts[0]).ok_or_else(|| err("bad start"))?;
    let (ey, em, is_now) = if parts.len() == 1 {
        (sy, sm, false)
    } else if parts[1].trim() == "now" {
        (9999, Some(12), true)
    } else {
        let (y, m) = parse_ym(parts[1]).ok_or_else(|| err("bad end"))?;
        (y, m, false)
    };
    // Lenient order check: only error when the end is unambiguously
    // before the start (so `2022-08 - 2022` is allowed).
    if !is_now {
        if ey < sy || (ey == sy && sm.is_some() && em.is_some() && em.unwrap() < sm.unwrap()) {
            return Err(err("end is before start"));
        }
    }
    let _ = is_range;
    let pretty = if is_range {
        let end_pretty = if is_now {
            "now".to_string()
        } else {
            pretty_ym(ey, em)
        };
        format!("{} - {}", pretty_ym(sy, sm), end_pretty)
    } else {
        pretty_ym(sy, sm)
    };
    // For `now` ranges store sy..9999; end_key() checks is_now first.
    Ok((pretty, DateRange {
        start_year: sy,
        start_month: sm,
        end_year: ey,
        end_month: em,
        is_now,
    }))
}

/// Parse the `--dates--` field of an entry, using the field's header line
/// for errors.
fn dates_of(path: &Path, map: &HashMap<String, (String, usize)>) -> Result<(String, DateRange), String> {
    let raw = required(path, map, "dates")?;
    let line = map.get("dates").map(|(_, l)| *l).unwrap_or(0);
    parse_dates(path, line, &raw)
}

/// Parse one experience/project file: `--title-- --company-- --dates--
/// --location-- --tags-- --bullets--`.
pub fn parse_experience(path: &Path, kind: &'static str) -> Result<Entry, String> {
    let text = read_file(path)?;
    let fields = parse_fields(path, &text)?;
    let map = index_fields(
        path,
        &fields,
        &["title", "company", "dates", "location", "tags", "bullets"],
    )?;
    let title = required(path, &map, "title")?;
    let company = required(path, &map, "company")?;
    let (dates, range) = dates_of(path, &map)?;
    let location = optional(&map, "location");
    let tags = parse_tags(&optional(&map, "tags"));
    let bullets_raw = required(path, &map, "bullets")?;
    let bullets = parse_list(&bullets_raw);
    if bullets.is_empty() {
        return Err(format!("{}: `--bullets--` has no bullet lines", path.display()));
    }
    Ok(Entry {
        id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("entry")
            .to_string(),
        kind,
        file: path.display().to_string(),
        title,
        company,
        dates,
        range,
        location,
        tags,
        bullets,
    })
}

/// Parse one education file: `--school-- --degree-- --dates-- --location--
/// --tags-- --bullets--`. Rendered with `school` in place of title and
/// `degree` in place of company via [`Entry::title`] / [`Entry::company`].
pub fn parse_education(path: &Path) -> Result<Entry, String> {
    let text = read_file(path)?;
    let fields = parse_fields(path, &text)?;
    let map = index_fields(
        path,
        &fields,
        &["school", "degree", "dates", "location", "tags", "bullets"],
    )?;
    let school = required(path, &map, "school")?;
    let degree = required(path, &map, "degree")?;
    let (dates, range) = dates_of(path, &map)?;
    let location = optional(&map, "location");
    let tags = parse_tags(&optional(&map, "tags"));
    let bullets_raw = required(path, &map, "bullets")?;
    let bullets = parse_list(&bullets_raw);
    if bullets.is_empty() {
        return Err(format!("{}: `--bullets--` has no bullet lines", path.display()));
    }
    Ok(Entry {
        id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("entry")
            .to_string(),
        kind: "education",
        file: path.display().to_string(),
        title: school,
        company: degree,
        dates,
        range,
        location,
        tags,
        bullets,
    })
}

/// Parse `pool/header.txt`: `--name-- --email--` required,
/// `--phone-- --links--` optional (each link one per line).
fn parse_header(path: &Path) -> Result<(String, String, String, Vec<String>), String> {
    let text = read_file(path)?;
    let fields = parse_fields(path, &text)?;
    let map = index_fields(path, &fields, &["name", "phone", "email", "links"])?;
    let name = required(path, &map, "name")?;
    let email = required(path, &map, "email")?;
    let phone = optional(&map, "phone");
    let links = map
        .get("links")
        .map(|(v, _)| parse_list(v))
        .unwrap_or_default();
    Ok((name, phone, email, links))
}

/// Parse `pool/skills.txt`: one named field per category, any name allowed
/// (`--languages--`, `--os-cloud--`, `--interests--`, ...). Order is
/// preserved; each value must be non-empty.
fn parse_skills(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = read_file(path)?;
    let fields = parse_fields(path, &text)?;
    // No fixed allow-list: the header is the category label. Still reject
    // duplicates, and require every value to be non-empty.
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for f in &fields {
        if seen.contains(&f.name) {
            return Err(format!(
                "{}: duplicate category `--{}--` at line {}",
                path.display(),
                f.name,
                f.line
            ));
        }
        seen.push(f.name.clone());
        if f.value.trim().is_empty() {
            return Err(format!(
                "{}: category `--{}--` at line {} is empty",
                path.display(),
                f.name,
                f.line
            ));
        }
        // Multi-line values collapse to one line; skill lines are prose.
        let value = f
            .value
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        out.push((title_case(&f.name), value));
    }
    if out.is_empty() {
        return Err(format!("{}: no skill categories found", path.display()));
    }
    Ok(out)
}

/// `os-cloud` -> `Os-cloud`? No: `os-cloud` -> `Os Cloud` (words titled).
fn title_case(s: &str) -> String {
    s.split(['-', '_'])
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn glob_txt(dir: &Path, what: &str) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("pool directory {} is missing", dir.display()));
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|en| en.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("txt"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no {what} files in {}", dir.display()));
    }
    Ok(paths)
}

/// Load the whole pool from `root/pool`. Any malformed file aborts the
/// build with a file:line error.
pub fn load_pool(root: &Path) -> Result<Pool, String> {
    let pool_dir = root.join("pool");
    let (name, phone, email, links) = parse_header(&pool_dir.join("header.txt"))?;
    let skills = parse_skills(&pool_dir.join("skills.txt"))?;

    let mut education = Vec::new();
    for p in glob_txt(&pool_dir.join("education"), "education")? {
        education.push(parse_education(&p)?);
    }
    let mut experience = Vec::new();
    for p in glob_txt(&pool_dir.join("experience"), "experience")? {
        experience.push(parse_experience(&p, "experience")?);
    }
    let mut projects = Vec::new();
    let proj_dir = pool_dir.join("projects");
    if proj_dir.is_dir() {
        for p in glob_txt(&proj_dir, "project")? {
            projects.push(parse_experience(&p, "project")?);
        }
    }

    Ok(Pool {
        name,
        phone,
        email,
        links,
        education,
        experience,
        projects,
        skills,
    })
}

/// Compact single-blob description of an entry, sent to the decision model
/// as `state` context. Deterministic for identical files.
pub fn describe(entry: &Entry) -> String {
    let mut s = format!(
        "{} @ {} ({})",
        entry.title, entry.company, entry.dates
    );
    if !entry.location.is_empty() {
        s.push_str(&format!(" [{}]", entry.location));
    }
    if !entry.tags.is_empty() {
        s.push_str(&format!(" (tags: {})", entry.tags.join(", ")));
    }
    s.push('\n');
    for b in &entry.bullets {
        s.push_str(&format!("- {b}\n"));
    }
    s.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// LaTeX rendering (template placeholders match template.tex).
// ---------------------------------------------------------------------------

pub fn latex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str(r"\&"),
            '%' => out.push_str(r"\%"),
            '$' => out.push_str(r"\$"),
            '#' => out.push_str(r"\#"),
            '_' => out.push_str(r"\_"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '~' => out.push_str(r"\textasciitilde{}"),
            '^' => out.push_str(r"\textasciircum{}"),
            '\\' => out.push_str(r"\textbackslash{}"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape a URL for the first argument of `\href{url}{text}`.
///
/// Only `%`, `#`, `&` are escaped, per hyperref's documentation for
/// `\href`: its first argument gets verbatim-ish catcodes, so `_`,
/// `~`, `^`, `$`, `{`, `}` must be left raw — hyperref passes them
/// to the URI unchanged, while e.g. `\$` in the URL would expand
/// to `\protect...` and corrupt the annotation. The three escapes
/// are the documented word-safe forms and verified to round-trip
/// to the raw bytes in the PDF `/URI` annotation.
fn latex_escape_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => out.push_str(r"\%"),
            '#' => out.push_str(r"\#"),
            '&' => out.push_str(r"\&"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render inline prose to LaTeX, preserving Markdown links.
///
/// `[text](url)` becomes `\href{url}{text}` (clickable in the PDF via
/// hyperref) with `text` escaped via [`latex_escape`] and `url` via
/// `latex_escape_url`. Text around the link is escaped normally.
/// Anything that is not a well-formed link — `[foo]` with no
/// `(url)`, `[](url)` with empty text, `(url)` with an empty or
/// whitespace-containing URL, or a `[` with no closing `]` — falls
/// back to literal (escaped) text. Brackets in the link text and
/// parentheses in the URL are matched with nesting so e.g.
/// `[see [inner]](url)` and `[wiki](https://en.wikipedia.org/wiki/A_(B))`
/// parse correctly.
pub fn render_inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // Balanced `]` search (link text may contain `[...]`).
            let mut j = i + 1;
            let mut depth = 1usize;
            let mut close: Option<usize> = None;
            while j < chars.len() {
                if chars[j] == '[' {
                    depth += 1;
                } else if chars[j] == ']' {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                j += 1;
            }
            if let Some(c) = close {
                if c > i + 1 && chars.get(c + 1) == Some(&'(') {
                    // Balanced `)` search (URLs may contain `(...)`).
                    let mut k = c + 2;
                    let mut pdepth = 1usize;
                    let mut close_paren: Option<usize> = None;
                    while k < chars.len() {
                        if chars[k] == '(' {
                            pdepth += 1;
                        } else if chars[k] == ')' {
                            pdepth -= 1;
                            if pdepth == 0 {
                                close_paren = Some(k);
                                break;
                            }
                        }
                        k += 1;
                    }
                    if let Some(cp) = close_paren {
                        let text: String = chars[i + 1..c].iter().collect();
                        let url: String = chars[c + 2..cp].iter().collect();
                        if !url.is_empty()
                            && !url.chars().any(|ch| ch.is_whitespace())
                            && !text.contains('\n')
                        {
                            out.push_str("\\href{");
                            out.push_str(&latex_escape_url(&url));
                            out.push_str("}{");
                            out.push_str(&latex_escape(&text));
                            out.push('}');
                            i = cp + 1;
                            continue;
                        }
                    }
                }
            }
        }
        // Literal character (or failed link candidate): escape normally.
        let ch = chars[i];
        let mut buf = [0u8; 4];
        out.push_str(&latex_escape(ch.encode_utf8(&mut buf)));
        i += 1;
    }
    out
}

fn render_entry(entry: &Entry) -> String {
    let mut s = format!(
        "\\entryHeading{{{}}}{{{}}}{{{}}}{{{}}}\n",
        render_inline(&entry.title),
        render_inline(&entry.company),
        latex_escape(&entry.dates),
        render_inline(&entry.location),
    );
    if !entry.bullets.is_empty() {
        s.push_str("\\begin{itemize}\n");
        for b in &entry.bullets {
            s.push_str(&format!("\\item {}\n", render_inline(b)));
        }
        s.push_str("\\end{itemize}\n");
    }
    s
}

/// Render the full `.tex` by substituting the template placeholders.
/// `exps`/`projs` are the selected entries (in ranking order); every
/// selected entry keeps all of its bullets.
pub fn render_tex(template: &str, pool: &Pool, exps: &[Entry], projs: &[Entry]) -> String {
    let mut parts = vec![pool.phone.clone(), pool.email.clone()];
    parts.extend(pool.links.clone());
    let contact = parts
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| render_inline(p))
        .collect::<Vec<_>>()
        .join(r" $\vert$ ");

    let edu_tex: String = pool.education.iter().map(render_entry).collect();
    let exp_tex: String = exps.iter().map(render_entry).collect();
    let proj_tex = if projs.is_empty() {
        String::new()
    } else {
        let mut s = String::from("\\section*{Projects}\n");
        for p in projs {
            s.push_str(&render_entry(p));
        }
        s
    };
    let skills_tex = pool
        .skills
        .iter()
        .map(|(label, v)| format!("{}: {}", latex_escape(label), render_inline(v)))
        .collect::<Vec<_>>()
        .join(" \\\\\n");

    template
        .replace("@@NAME@@", &render_inline(&pool.name))
        .replace("@@CONTACT@@", &contact)
        .replace("@@EDUCATION@@", &edu_tex)
        .replace("@@EXPERIENCE@@", &exp_tex)
        .replace("@@PROJECTS@@", &proj_tex)
        .replace("@@SKILLS@@", &skills_tex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_experience_any_order() {
        let dir = std::env::temp_dir().join(format!("pool-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.txt");
        std::fs::write(
            &p,
            "--bullets--\n did a thing \n--title--\n Dev \n--company--\n Acme \n--dates--\n 2024 \n",
        )
        .unwrap();
        let e = parse_experience(&p, "experience").unwrap();
        assert_eq!(e.title, "Dev");
        assert_eq!(e.company, "Acme");
        assert_eq!(e.dates, "2024");
        assert_eq!(e.bullets, vec!["did a thing"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_required_is_an_error() {
        let dir = std::env::temp_dir().join(format!("pool-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.txt");
        std::fs::write(&p, "--title--\n Dev \n--dates--\n 2024 \n--bullets--\n x \n").unwrap();
        let err = parse_experience(&p, "experience").unwrap_err();
        assert!(err.contains("missing required field `--company--`"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bare_dashes_are_an_error_not_a_field() {
        let dir = std::env::temp_dir().join(format!("pool-test3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.txt");
        std::fs::write(&p, "--title--\nDev\n--\nAcme\n").unwrap();
        let err = parse_experience(&p, "experience").unwrap_err();
        assert!(err.contains("not a valid field header"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_and_unknown_are_errors() {
        let dir = std::env::temp_dir().join(format!("pool-test4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.txt");
        std::fs::write(
            &p,
            "--title--\n Dev \n--title--\n Twice \n--company--\n Acme \n--dates--\n 2024 \n--bullets--\n x \n",
        )
        .unwrap();
        assert!(parse_experience(&p, "experience").unwrap_err().contains("duplicate field"));
        std::fs::write(
            &p,
            "--title--\n Dev \n--company--\n Acme \n--dates--\n 2024 \n--bogus--\n hi \n--bullets--\n x \n",
        )
        .unwrap();
        assert!(parse_experience(&p, "experience").unwrap_err().contains("unknown field"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates_pretty_and_keys() {
        let p = Path::new("dates.txt");
        let (pretty, r) = parse_dates(p, 1, "2024 - 2026-08").unwrap();
        assert_eq!(pretty, "2024 - Aug 2026");
        assert_eq!(r.start_key(), (2024, 0));
        assert_eq!(r.end_key(), (2026, 8));
        let (pretty, r) = parse_dates(p, 1, "2023 - now").unwrap();
        assert_eq!(pretty, "2023 - now");
        assert!(r.is_now);
        let (pretty, _) = parse_dates(p, 1, "2019-09 - 2023-12").unwrap();
        assert_eq!(pretty, "Sep 2019 - Dec 2023");
    }

    #[test]
    fn dates_free_text_rejected() {
        let p = Path::new("dates.txt");
        assert!(parse_dates(p, 1, "Summer 2022 (2 months)").is_err());
        assert!(parse_dates(p, 1, "2024 -- Aug 2026").is_err());
        assert!(parse_dates(p, 1, "2022-13").is_err());
        assert!(parse_dates(p, 1, "2023 - 2020").is_err());
        assert!(parse_dates(p, 1, "2020-03 - 2020-01").is_err());
        assert!(parse_dates(p, 1, "2022-08 - 2022").is_ok());
    }

    #[test]
    fn markdown_link_renders_as_href() {
        // The exact bullet from pool/projects/agent-o-rama.txt.
        assert_eq!(
            render_inline(
                "[Keynote demo](https://youtu.be/mNLWtM3Iya4?t=2017) at clojure/conj 2025"
            ),
            r"\href{https://youtu.be/mNLWtM3Iya4?t=2017}{Keynote demo} at clojure/conj 2025"
        );
    }

    #[test]
    fn markdown_link_escapes_text_and_url() {
        assert_eq!(
            render_inline("[a & b_c](https://example.com/x_y#frag) % done"),
            r"\href{https://example.com/x_y\#frag}{a \& b\_c} \% done"
        );
        // Hyperref verbatim URL arg: `$`, `^`, `~` stay raw in the URL.
        assert_eq!(
            render_inline("[x](https://example.com/a$b^c~d)"),
            r"\href{https://example.com/a$b^c~d}{x}"
        );
        assert_eq!(
            render_inline("see [one](https://a.b) and [two](https://c.d)"),
            r"see \href{https://a.b}{one} and \href{https://c.d}{two}"
        );
    }

    #[test]
    fn markdown_link_nesting_and_fallback() {
        // Balanced parens in the URL, balanced brackets in the text.
        assert_eq!(
            render_inline("[wiki](https://en.wikipedia.org/wiki/A_(B))"),
            r"\href{https://en.wikipedia.org/wiki/A_(B)}{wiki}"
        );
        assert_eq!(
            render_inline("[see [v2]](https://example.com)"),
            r"\href{https://example.com}{see [v2]}"
        );
        // Not links: no parens, empty text, empty url, space in url.
        assert_eq!(render_inline("[not a link]"), "[not a link]");
        assert_eq!(
            render_inline("[](https://example.com)"),
            "[](https://example.com)"
        );
        assert_eq!(render_inline("[x]()"), "[x]()");
        assert_eq!(
            render_inline("[x](https://a.b/c d)"),
            "[x](https://a.b/c d)"
        );
        assert_eq!(render_inline("[x](https://a.b"), "[x](https://a.b");
        // Plain text still escapes as before.
        assert_eq!(render_inline("50% & _x_"), r"50\% \& \_x\_");
    }

    #[test]
    fn render_entry_keeps_markdown_link_as_href() {
        let e = Entry {
            id: "agent-o-rama".to_string(),
            kind: "project",
            file: "agent-o-rama.txt".to_string(),
            title: "agent-o-rama debugger UI".to_string(),
            company: "Red Planet Labs".to_string(),
            dates: "2024 - 2025".to_string(),
            range: DateRange {
                start_year: 2024,
                start_month: None,
                end_year: 2025,
                end_month: None,
                is_now: false,
            },
            location: String::new(),
            tags: vec![],
            bullets: vec![
                "[Keynote demo](https://youtu.be/mNLWtM3Iya4?t=2017) at clojure/conj 2025"
                    .to_string(),
            ],
        };
        let tex = render_entry(&e);
        assert!(
            tex.contains(r"\href{https://youtu.be/mNLWtM3Iya4?t=2017}{Keynote demo}"),
            "{tex}"
        );
        assert!(!tex.contains("[Keynote demo]("), "{tex}");
    }
}
