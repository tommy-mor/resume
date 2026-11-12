//! resume — build a one-page LaTeX resume from the pool, ranked by
//! pairwise OpenRouter decision-model judgments.
//!
//! Flow: parse the named-header pool (`pool/`) → deal pairs with the
//! muse-tricks dealer (`pair.rs`) → judge each pair with
//! `google/gemini-decision-1` on OpenRouter (`decisions.rs`, the only
//! AI path) → settle the vote graph with the muse-tricks random walk
//! (`ranking.rs`) → order experience newest-first by start date
//! (projects keep salience order) → also rank skill atoms and
//! coursework atoms pairwise and keep only the top hits → render
//! `template.tex` with every kept entry → compile with
//! tectonic/pdflatex → only if the PDF overflows one page, drop the
//! least-salient entry and rebuild until it fits. Ranking exists
//! solely to pick the trim order.
//!
//! There is no local scoring and no other provider: without
//! `OPENROUTER_KEY` the build fails before compiling anything.

mod decisions;
mod pair;
mod pool;
mod ranking;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use decisions::DecisionsClient;
use pool::{describe, render_tex, skill_atoms, split_rank_items, Entry, Pool};
use ranking::{rank_groups, MAX_ITERS, TOL};

const ROOT: &str = ".";
const DEFAULT_MODEL: &str = decisions::DEFAULT_MODEL;
/// Skills survive: top N atoms by pairwise relevance (keeps the
/// section to ~1 line instead of listing every tool).
const SKILLS_KEEP: usize = 8;
/// Votes cap for the skills tournament (bridging connects in n-1;
/// a full pairwise over ~19 atoms would be 171 calls).
const SKILLS_VOTES: usize = 40;
/// Courses survive: top N atoms so coursework fits one line.
const COURSES_KEEP: usize = 4;

#[derive(Parser)]
#[command(
    name = "resume",
    about = "Pairwise-ranked one-page resume builder (Rust). Ranking needs OPENROUTER_KEY via env/.env."
)]
struct Args {
    /// Job description .txt to tailor against.
    #[arg(long)]
    job: PathBuf,
    /// Output PDF path.
    #[arg(long, default_value = "resume.pdf")]
    out: PathBuf,
    /// OpenRouter Decisions model id. Always served by OpenRouter.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Cap on pairwise decisions for the combined tournament. Default
    /// is the full tournament `n*(n-1)/2`. Smaller values must still
    /// leave the vote graph connected or the build fails (no silent
    /// partial ranking).
    #[arg(long)]
    max_votes: Option<usize>,
    /// Include everything without AI ranking (may spill past one page).
    #[arg(long, visible_alias = "no-rank")]
    all: bool,
    /// Print the ranking without compiling.
    #[arg(long)]
    list_only: bool,
    /// Also write the generated .tex to ./resume.tex.
    #[arg(long)]
    keep_tex: bool,
}

fn canon(a: u32, b: u32) -> (u32, u32) {
    if a < b { (a, b) } else { (b, a) }
}

fn flush_stderr() {
    let _ = std::io::stderr().flush();
}

/// Live snapshot of the ranking after `votes_done` decisions.
///
/// Recomputes the random-walk scores from the edges seen so far. When the
/// vote graph is still disconnected the scores are only comparable
/// inside each component, so components are printed separately and
/// never-voted singletons are collapsed into one `unvoted:` line.
/// Returned as text (no print) so it can be asserted in tests.
fn interim_ranking_text(
    kind: &str,
    entries: &[Entry],
    voted: &HashSet<(usize, usize)>,
    edges: &[(usize, usize, f64)],
    votes_done: usize,
    budget: usize,
) -> String {
    let groups = rank_groups(entries.len(), voted, edges, MAX_ITERS, TOL);
    let mut s = String::new();
    if groups.len() == 1 {
        s.push_str(&format!(
            "  [ranking {kind} @ {votes_done}/{budget} votes] ({} entries)\n",
            entries.len()
        ));
        for (rank, (idx, score)) in groups[0].ranked.iter().enumerate() {
            let e = &entries[*idx];
            s.push_str(&format!(
                "    {}. {:.2}  {}: {} @ {}\n",
                rank + 1,
                score,
                e.id,
                e.title,
                e.company
            ));
        }
    } else {
        s.push_str(&format!(
            "  [ranking {kind} @ {votes_done}/{budget} votes] partial — {} components\n",
            groups.len()
        ));
        let mut unvoted: Vec<String> = Vec::new();
        for (gi, g) in groups.iter().enumerate() {
            if g.members.len() == 1 {
                unvoted.push(entries[g.members[0]].id.clone());
                continue;
            }
            s.push_str(&format!(
                "    component {} ({} entries, {} votes):\n",
                gi + 1,
                g.members.len(),
                g.pairs
            ));
            for (rank, (idx, score)) in g.ranked.iter().enumerate() {
                let e = &entries[*idx];
                s.push_str(&format!(
                    "      {}. {:.2}  {}: {} @ {}\n",
                    rank + 1,
                    score,
                    e.id,
                    e.title,
                    e.company
                ));
            }
        }
        if !unvoted.is_empty() {
            s.push_str(&format!("    unvoted ({}): {}\n", unvoted.len(), unvoted.join(", ")));
        }
    }
    s.trim_end().to_string()
}

fn print_interim_ranking(
    kind: &str,
    entries: &[Entry],
    voted_u32: &HashSet<(u32, u32)>,
    edges: &[(usize, usize, f64)],
    votes_done: usize,
    budget: usize,
) {
    let voted_usize: HashSet<(usize, usize)> = voted_u32
        .iter()
        .map(|&(a, b)| (a as usize, b as usize))
        .collect();
    eprintln!("{}", interim_ranking_text(kind, entries, &voted_usize, edges, votes_done, budget));
    flush_stderr();
}

/// Rank one kind (experience or projects) by sequential pairwise
/// decisions. Returns the entries best-first with their settled scores.
/// With fewer than two entries there is nothing to compare: the single
/// entry scores `1.0` with no API call.
/// Progress is printed to stderr as it ranks: the pair being judged
/// (flushed) before the blocking request, then the result and the live
/// interim ranking after each vote — so a slow or failed request still
/// shows which comparison was in flight instead of silence.
fn rank_kind(
    kind: &str,
    entries: &[Entry],
    job_text: &str,
    client: &DecisionsClient,
    max_votes: Option<usize>,
) -> Result<Vec<(f64, Entry)>, String> {
    let n = entries.len();
    if n == 0 {
        eprintln!("[rank] {kind}: no entries");
        flush_stderr();
        return Ok(vec![]);
    }
    if n == 1 {
        eprintln!("[rank] {kind}: single entry {}, score 1.00 (no votes)", entries[0].id);
        flush_stderr();
        return Ok(vec![(1.0, entries[0].clone())]);
    }
    let n32 = n as u32;
    let budget = max_votes.unwrap_or((n * (n - 1)) / 2);
    // Display denominator: the tournament has only `n*(n-1)/2` pairs, so a
    // larger --max-votes cap for a small kind (e.g. 3 projects) would
    // otherwise print "1/7" for a 3-pair tournament.
    let total_pairs = (n * (n - 1)) / 2;
    let display_total = budget.min(total_pairs);
    let texts: Vec<String> = entries.iter().map(describe).collect();
    eprintln!("[rank] {kind}: ranking {n} entries over {display_total} votes (cap {budget})");
    flush_stderr();

    let mut voted: HashSet<(u32, u32)> = HashSet::new();
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    let mut current: Option<(u32, u32)> = None;
    let mut round: usize = 0;

    while let Some((a, b)) = pair::suggest_next_pair(n32, &voted, &HashSet::new(), current, None) {
        if round >= budget {
            break;
        }
        // Deterministic position-bias hedge: alternate which side is
        // presented as A on consecutive rounds. The voted set stays
        // canonical; only the prompt order flips.
        let (pa, pb) = if round % 2 == 0 { (a, b) } else { (b, a) };
        // Announce before the blocking request (and flush): without
        // this a slow/failed vote looks like silence, not progress.
        eprintln!(
            "[vote {}/{}] ({kind}) judging {} vs {} ...",
            round + 1,
            display_total,
            entries[a as usize].id,
            entries[b as usize].id
        );
        flush_stderr();
        let t0 = Instant::now();
        let (pa_share, pb_share) = client
            .decide_pair(job_text, &texts[pa as usize], &texts[pb as usize])
            .map_err(|e| {
                format!(
                    "({kind} vote {}/{}: {} vs {} presented as {} vs {}) {e}",
                    round + 1,
                    display_total,
                    entries[a as usize].id,
                    entries[b as usize].id,
                    entries[pa as usize].id,
                    entries[pb as usize].id,
                )
            })?;
        let secs = t0.elapsed().as_secs_f64();
        // Edges flow loser -> winner weighted by the winner's share
        // (muse-tricks orientation): presented-A's share lands on
        // (presented-B -> presented-A) and vice versa.
        edges.push((pb as usize, pa as usize, pa_share));
        edges.push((pa as usize, pb as usize, pb_share));
        let pair_key = canon(a, b);
        current = Some(pair_key);
        voted.insert(pair_key);
        let winner = if pa_share > pb_share { pa } else { pb };
        let (w_share, l_share) = if pa_share > pb_share {
            (pa_share, pb_share)
        } else {
            (pb_share, pa_share)
        };
        eprintln!(
            "[vote {}/{}] ({kind}) {} vs {}: presented-a {:.2} presented-b {:.2} -> {} leads {:.2}-{:.2} ({:.1}s)",
            round + 1,
            display_total,
            entries[a as usize].id,
            entries[b as usize].id,
            pa_share,
            pb_share,
            entries[winner as usize].id,
            w_share,
            l_share,
            secs
        );
        flush_stderr();
        round += 1;
        print_interim_ranking(kind, entries, &voted, &edges, round, display_total);
    }

    let voted_usize: HashSet<(usize, usize)> = voted
        .iter()
        .map(|&(a, b)| (a as usize, b as usize))
        .collect();
    let groups = rank_groups(n, &voted_usize, &edges, MAX_ITERS, TOL);
    if groups.len() != 1 {
        return Err(format!(
            "{kind}: vote graph has {} connected components after {round} decisions \
             (budget {budget}); ranking is undefined across components — \
             raise --max-votes (default is the full {}-pair tournament)",
            groups.len(),
            n * (n - 1) / 2
        ));
    }
    let ranked: Vec<(f64, Entry)> = groups[0]
        .ranked
        .iter()
        .map(|&(idx, score)| (score, entries[idx].clone()))
        .collect();
    eprintln!("[rank] {kind}: done ({round}/{display_total} votes), final ranking:");
    for (rank, (score, e)) in ranked.iter().enumerate() {
        eprintln!("    {}. {:.2}  {}: {} @ {}", rank + 1, score, e.id, e.title, e.company);
    }
    flush_stderr();
    Ok(ranked)
}

/// Order experience newest-first by start date (ties: later end first,
/// then `id` for determinism). Salience decides the trim order on
/// overflow; date decides where experience appears.
fn sort_by_date_desc(items: &mut [(f64, Entry)]) {
    items.sort_by(|(_, a), (_, b)| {
        b.range
            .start_key()
            .cmp(&a.range.start_key())
            .then_with(|| b.range.end_key().cmp(&a.range.end_key()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Index of the next entry to trim: the least-salient (lowest score).
/// Ties go to the oldest start date (year-only earliest, then `id`),
/// so a near-equal item is trimmed from the older end. Returns `None`
/// for an empty list. Scores come from the combined tournament, so
/// they are comparable across experience and projects.
fn trim_victim(items: &[(f64, Entry)]) -> Option<usize> {
    let mut victim: Option<usize> = None;
    for (i, (score, entry)) in items.iter().enumerate() {
        match victim {
            None => victim = Some(i),
            Some(v) => {
                let (v_score, v_entry) = &items[v];
                let score = *score;
                let v_score = *v_score;
                // NaN scores sort last (never picked while a real score exists).
                let ord = score.partial_cmp(&v_score).unwrap_or(std::cmp::Ordering::Greater);
                let worse = ord == std::cmp::Ordering::Less
                    || (ord == std::cmp::Ordering::Equal
                        && (entry.range.start_key() < v_entry.range.start_key()
                            || (entry.range.start_key() == v_entry.range.start_key()
                                && entry.id < v_entry.id)));
                if worse {
                    victim = Some(i);
                }
            }
        }
    }
    victim
}

/// Which kind to trim next, and which entry in it: the globally
/// least-salient of the two kinds (combined-tournament scores).
/// `None` when both are empty. Returns `(is_experience, index)`.
fn global_trim_victim(exp: &[(f64, Entry)], proj: &[(f64, Entry)]) -> Option<(bool, usize)> {
    match (trim_victim(exp), trim_victim(proj)) {
        (None, None) => None,
        (Some(e), None) => Some((true, e)),
        (None, Some(p)) => Some((false, p)),
        (Some(e), Some(p)) => {
            let (e_score, e_entry) = &exp[e];
            let (p_score, p_entry) = &proj[p];
            // Lower score trims first; ties go to the older start.
            let e_older = (e_entry.range.start_key(), &e_entry.id)
                < (p_entry.range.start_key(), &p_entry.id);
            let drop_exp = e_score < p_score || (e_score == p_score && e_older);
            if drop_exp {
                Some((true, e))
            } else {
                Some((false, p))
            }
        }
    }
}

/// One synthetic entry per atomic skill/course for pairwise ranking.
/// `id` is the display slug; `title` is the item; `company` is the
/// category/school context the judge sees.
fn synthetic_entry(id: String, title: String, context: String, kind: &'static str) -> Entry {
    Entry {
        id,
        kind,
        file: String::new(),
        title: title.clone(),
        company: context,
        dates: String::new(),
        range: pool::DateRange {
            start_year: 2020,
            start_month: None,
            end_year: 2020,
            end_month: None,
            is_now: false,
        },
        location: String::new(),
        tags: vec![],
        bullets: vec![title],
    }
}

/// Rank atomic skills against the job and keep the top `SKILLS_KEEP`,
/// regrouped under their original categories in salience order.
fn rank_skills(
    skills: &[(String, String)],
    job_text: &str,
    client: &DecisionsClient,
) -> Result<Vec<(String, String)>, String> {
    let atoms = skill_atoms(skills);
    if atoms.len() < 2 {
        return Ok(skills.to_vec());
    }
    let entries: Vec<Entry> = atoms
        .iter()
        .enumerate()
        .map(|(i, (cat, item))| {
            synthetic_entry(format!("skill-{i}"), item.clone(), cat.clone(), "skill")
        })
        .collect();
    let ranked = rank_kind("skills", &entries, job_text, client, Some(SKILLS_VOTES))?;
    let top: Vec<Entry> = ranked.iter().take(SKILLS_KEEP).map(|(_, e)| e.clone()).collect();
    // Group by original category order, salience order inside.
    let mut out: Vec<(String, String)> = Vec::new();
    for (label, _) in skills {
        let kept: Vec<String> = top
            .iter()
            .filter(|e| &e.company == label)
            .map(|e| e.title.clone())
            .collect();
        if !kept.is_empty() {
            out.push((label.clone(), kept.join(", ")));
        }
    }
    Ok(out)
}

/// Rank coursework atoms and keep the top `COURSES_KEEP` on one line.
/// Each education entry keeps a single `Relevant coursework: ...` bullet.
fn rank_courses(
    education: &[Entry],
    job_text: &str,
    client: &DecisionsClient,
) -> Result<Vec<Entry>, String> {
    // Collect atoms across all education entries.
    let mut atoms: Vec<String> = Vec::new();
    for e in education {
        for b in &e.bullets {
            for item in split_rank_items(b) {
                let key = item.trim().to_lowercase();
                if !key.is_empty() && !atoms.iter().any(|a: &String| a.to_lowercase() == key) {
                    atoms.push(item);
                }
            }
        }
    }
    if atoms.len() < 2 {
        return Ok(education.to_vec());
    }
    let entries: Vec<Entry> = atoms
        .iter()
        .enumerate()
        .map(|(i, item)| {
            synthetic_entry(format!("course-{i}"), item.clone(), "Coursework".to_string(), "course")
        })
        .collect();
    let ranked = rank_kind("courses", &entries, job_text, client, None)?;
    let top: Vec<String> = ranked
        .iter()
        .take(COURSES_KEEP)
        .map(|(_, e)| e.title.clone())
        .collect();
    Ok(education
        .iter()
        .map(|e| {
            let mut next = e.clone();
            next.bullets = vec![format!("Relevant coursework: {}", top.join(", "))];
            next
        })
        .collect())
}

/// Sort criterion shared by both selection paths. Wraps [`rank_kind`]
/// for the AI path; `--all` skips the API and returns every entry,
/// with experience ordered newest-first by date (projects keep file
/// order — there is no salience to order by without the API).
fn select(
    pool: &Pool,
    job_text: &str,
    args: &Args,
) -> Result<(Vec<(f64, Entry)>, Vec<(f64, Entry)>, Vec<(String, String)>, Vec<Entry>), String> {
    if args.all {
        let mut exps = pool
            .experience
            .iter()
            .map(|e| (0.0, e.clone()))
            .collect::<Vec<_>>();
        sort_by_date_desc(&mut exps);
        let projs = pool
            .projects
            .iter()
            .map(|e| (0.0, e.clone()))
            .collect::<Vec<_>>();
        return Ok((exps, projs, pool.skills.clone(), pool.education.clone()));
    }
    // One client per build; constructed lazily so `--all` needs no key.
    let client = DecisionsClient::from_env(&args.model)?;
    eprintln!("[info] ranking with {} via OpenRouter Decisions API", client.model());
    // One combined tournament over experience + projects so a single
    // score scale orders the overflow trims (separate tournaments
    // would make cross-kind cuts incomparable).
    let mut combined: Vec<Entry> = Vec::with_capacity(pool.experience.len() + pool.projects.len());
    combined.extend(pool.experience.iter().cloned());
    combined.extend(pool.projects.iter().cloned());
    let ranked = rank_kind("combined", &combined, job_text, &client, args.max_votes)?;
    let (mut exps, mut projs): (Vec<(f64, Entry)>, Vec<(f64, Entry)>) = (Vec::new(), Vec::new());
    for item in ranked {
        if item.1.kind == "project" {
            projs.push(item);
        } else {
            exps.push(item);
        }
    }
    // Experience display re-orders newest-first; projects keep
    // salience order from the tournament.
    sort_by_date_desc(&mut exps);
    let skills = rank_skills(&pool.skills, job_text, &client)?;
    let education = rank_courses(&pool.education, job_text, &client)?;
    Ok((exps, projs, skills, education))
}

fn print_selection(title: &str, items: &[(f64, Entry)]) {
    println!("=== {title} ===");
    for (score, e) in items {
        println!(
            "{:5.2}  {:18}  {} @ {}  [{} bullets]",
            score, e.dates, e.title, e.company, e.bullets.len()
        );
    }
}

/// Whether `cmd` is an executable on PATH (no spawn, std-only).
fn which(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let p = dir.join(cmd);
        p.is_file()
            && std::fs::metadata(&p)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    })
}

fn compile_tex(
    tex_text: &str,
    out_pdf: &Path,
    keep_tex: bool,
    root: &Path,
) -> Result<bool, String> {
    let build_dir = root.join(".build");
    std::fs::create_dir_all(&build_dir).map_err(|e| format!(".build: {e}"))?;
    let tex_path = build_dir.join("resume.tex");
    std::fs::write(&tex_path, tex_text).map_err(|e| format!("{}: {e}", tex_path.display()))?;
    if keep_tex {
        std::fs::write(root.join("resume.tex"), tex_text).map_err(|e| format!("resume.tex: {e}"))?;
    }
    // Honour ~/.local/bin + ~/.cargo/bin for tectonic.
    if let Some(home) = std::env::var_os("HOME") {
        let mut new_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect::<Vec<_>>();
        for d in [
            Path::new(&home).join(".local/bin"),
            Path::new(&home).join(".cargo/bin"),
        ] {
            if d.is_dir() && !new_path.contains(&d) {
                new_path.push(d);
            }
        }
        if let Ok(joined) = std::env::join_paths(new_path) {
            std::env::set_var("PATH", joined);
        }
    }
    let has_tectonic = which("tectonic");
    let has_pdflatex = which("pdflatex");
    if !has_tectonic && !has_pdflatex {
        return Err(format!(
            "no LaTeX engine found (neither tectonic nor pdflatex on PATH). \
             Install texlive or tectonic, then re-run. .tex left at {}",
            tex_path.display()
        ));
    }
    let status = if has_tectonic {
        std::process::Command::new("tectonic")
            .arg(&tex_path)
            .arg("--outdir")
            .arg(&build_dir)
            .output()
            .map_err(|e| format!("run tectonic: {e}"))?
    } else {
        let mut last = None;
        for _ in 0..2 {
            // Two passes for hyperref stability.
            let o = std::process::Command::new("pdflatex")
                .arg("-interaction=nonstopmode")
                .arg("-halt-on-error")
                .arg("-output-directory")
                .arg(&build_dir)
                .arg(&tex_path)
                .output()
                .map_err(|e| format!("run pdflatex: {e}"))?;
            last = Some(o);
            if !last.as_ref().unwrap().status.success() {
                break;
            }
        }
        last.unwrap()
    };
    if !status.status.success() {
        let stdout = String::from_utf8_lossy(&status.stdout).to_string();
        let stderr = String::from_utf8_lossy(&status.stderr).to_string();
        eprintln!("{}{}", tail(&stdout, 3000), tail(&stderr, 3000));
        return Err("LaTeX compilation failed".to_string());
    }
    let pdf = build_dir.join("resume.pdf");
    std::fs::copy(&pdf, out_pdf).map_err(|e| format!("copy PDF to {}: {e}", out_pdf.display()))?;
    println!("[ok] wrote {} ({} KB)", out_pdf.display(), pdf_bytes(out_pdf));
    // Page check via pdfinfo when available; unknown counts as ok.
    Ok(page_is_one(out_pdf))
}

fn pdf_bytes(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len() / 1024).unwrap_or(0)
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

fn page_is_one(pdf: &Path) -> bool {
    let Ok(out) = std::process::Command::new("pdfinfo").arg(pdf).output() else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Pages:") {
            let pages = rest.trim();
            eprintln!("[info] pages: {pages}");
            return pages == "1";
        }
    }
    true
}

fn run() -> Result<(), String> {
    let args = Args::parse();
    let root = PathBuf::from(ROOT);
    let pool = pool::load_pool(&root)?;
    let job_text = std::fs::read_to_string(&args.job)
        .map_err(|e| format!("{}: {e}", args.job.display()))?;
    if job_text.trim().is_empty() {
        return Err(format!("{}: job description is empty", args.job.display()));
    }
    let template = std::fs::read_to_string(root.join("template.tex"))
        .map_err(|e| format!("template.tex: {e}"))?;

    let (ranked_exps, ranked_projs, ranked_skills, ranked_education) = select(&pool, &job_text, &args)?;
    // Pool clone carrying the job-trimmed skills/classwork.
    let mut pool_render = pool.clone();
    pool_render.skills = ranked_skills;
    pool_render.education = ranked_education;
    // No pre-filter: every ranked entry is included. Ranking only
    // orders the shrink-to-fit trims below (nothing is dropped
    // unless the compiled PDF overflows one page).
    let mut exps_scored: Vec<(f64, Entry)> = ranked_exps.clone();
    sort_by_date_desc(&mut exps_scored);
    let mut projs_scored: Vec<(f64, Entry)> = ranked_projs.clone();

    println!(
        "(experience: {} entries included, displayed newest-first; trimming only if PDF overflows)",
        exps_scored.len()
    );
    print_selection("included experience", &exps_scored);
    print_selection("included projects", &projs_scored);
    println!("=== trimmed skills ({} categories) ===", pool_render.skills.len());
    for (label, v) in &pool_render.skills {
        println!("  {label}: {v}");
    }
    for e in &pool_render.education {
        for b in &e.bullets {
            println!("  {b}");
        }
    }
    if args.list_only {
        return Ok(());
    }

    // Shrink-to-fit: render every entry with all its bullets first.
    // Only while the PDF is >1 page, drop the least-salient entry
    // (ranking is the trim order) and rebuild.
    let render_entries = |scored: &[(f64, Entry)]| -> Vec<Entry> {
        scored.iter().map(|(_, e)| e.clone()).collect()
    };
    let mut tex = render_tex(&template, &pool_render, &render_entries(&exps_scored), &render_entries(&projs_scored));
    let mut pages_ok = compile_tex(&tex, &args.out, args.keep_tex || args.all, &root)?;
    if !args.all {
        while !pages_ok && (!exps_scored.is_empty() || !projs_scored.is_empty()) {
            // Combined scores: drop the single globally least-salient
            // entry per rebuild, whatever its kind.
            let (is_exp, idx) = global_trim_victim(&exps_scored, &projs_scored).expect("non-empty");
            if is_exp {
                let (score, dropped) = exps_scored.remove(idx);
                eprintln!(
                    "[info] >1 page, dropping least-salient experience: {} (score {:.2}) and rebuilding",
                    dropped.id, score
                );
            } else {
                let (score, dropped) = projs_scored.remove(idx);
                eprintln!(
                    "[info] >1 page, dropping least-salient project: {} (score {:.2}) and rebuilding",
                    dropped.id, score
                );
            }
            tex = render_tex(&template, &pool_render, &render_entries(&exps_scored), &render_entries(&projs_scored));
            pages_ok = compile_tex(&tex, &args.out, args.keep_tex, &root)?;
        }
        if !pages_ok {
            eprintln!(
                "[warn] still >1 page with {} experience + {} projects; pool is too large for one page — remove entries from pool/",
                exps_scored.len(),
                projs_scored.len()
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("resume: error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_orders_pairs() {
        assert_eq!(canon(1, 0), (0, 1));
        assert_eq!(canon(0, 1), (0, 1));
    }

    fn entry_with(id: &str, start: (u32, Option<u8>), end: (u32, Option<u8>), now: bool) -> Entry {
        Entry {
            id: id.to_string(),
            kind: "experience",
            file: String::new(),
            title: id.to_string(),
            company: String::new(),
            dates: String::new(),
            range: pool::DateRange {
                start_year: start.0,
                start_month: start.1,
                end_year: end.0,
                end_month: end.1,
                is_now: now,
            },
            location: String::new(),
            tags: vec![],
            bullets: vec![],
        }
    }

    #[test]
    fn experience_sorts_newest_first_by_start() {
        // Scores are salience; date order must ignore them.
        let mut items = vec![
            (0.99, entry_with("old", (2020, Some(6)), (2020, Some(12)), false)),
            (0.01, entry_with("new", (2024, None), (2026, Some(8)), false)),
            (0.50, entry_with("mid-now", (2023, None), (9999, Some(12)), true)),
        ];
        sort_by_date_desc(&mut items);
        let ids: Vec<&str> = items.iter().map(|(_, e)| e.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "mid-now", "old"]);
    }

    fn entries3() -> Vec<Entry> {
        vec![
            entry_with("a", (2020, None), (2020, None), false),
            entry_with("b", (2020, None), (2020, None), false),
            entry_with("c", (2020, None), (2020, None), false),
        ]
    }

    #[test]
    fn interim_ranking_shows_live_partial_order() {
        // 0 beats 1 (0.75); 2 unvoted -> partial with one component + singleton.
        let entries = entries3();
        let voted: HashSet<(usize, usize)> = HashSet::from([(0, 1)]);
        let edges = vec![(1, 0, 0.75), (0, 1, 0.25)];
        let text = interim_ranking_text("experience", &entries, &voted, &edges, 1, 3);
        assert!(text.contains("[ranking experience @ 1/3 votes]"), "{text}");
        assert!(text.contains("partial — 2 components"), "{text}");
        assert!(text.contains("component 1 (2 entries, 1 votes)"), "{text}");
        // Winner ranks first inside its component.
        let first = text.find("a: a @").unwrap();
        let second = text.find("b: b @").unwrap();
        assert!(first < second, "{text}");
        assert!(text.contains("unvoted (1): c"), "{text}");
    }

    #[test]
    fn interim_ranking_connected_shows_full_order() {
        // 2 beats 1, 1 beats 0 -> 2 > 1 > 0 in one component.
        let entries = entries3();
        let voted: HashSet<(usize, usize)> = HashSet::from([(0, 1), (1, 2)]);
        let edges = vec![
            (0, 1, 0.67),
            (1, 0, 0.33),
            (1, 2, 0.67),
            (2, 1, 0.33),
        ];
        let text = interim_ranking_text("experience", &entries, &voted, &edges, 2, 3);
        assert!(text.contains("[ranking experience @ 2/3 votes] (3 entries)"), "{text}");
        assert!(!text.contains("partial"), "{text}");
        let pos_c = text.find("c: c @").unwrap();
        let pos_b = text.find("b: b @").unwrap();
        let pos_a = text.find("a: a @").unwrap();
        assert!(pos_c < pos_b && pos_b < pos_a, "{text}");
    }

    #[test]
    fn trim_victim_picks_least_salient_then_oldest() {
        // Scores decide; among equal lows the oldest start goes first.
        let items = vec![
            (0.90, entry_with("new-high", (2024, None), (2026, None), false)),
            (0.00, entry_with("mid-low", (2020, None), (2020, None), false)),
            (0.00, entry_with("old-low", (2019, None), (2019, None), false)),
        ];
        assert_eq!(trim_victim(&items).unwrap(), 2, "oldest of the tied lows");
        assert_eq!(trim_victim(&items[0..1]).unwrap(), 0);
        assert_eq!(trim_victim(&[]), None);
    }

    #[test]
    fn global_trim_picks_lowest_score_across_kinds() {
        let mut proj = entry_with("proj-low", (2023, None), (2023, None), false);
        proj.kind = "project";
        let exp = vec![
            (0.40, entry_with("exp-mid", (2020, None), (2020, None), false)),
            (0.30, entry_with("exp-high", (2024, None), (2024, None), false)),
        ];
        let projs = vec![(0.05, proj)];
        // Project (0.05) is below every experience score: it goes first.
        assert_eq!(global_trim_victim(&exp, &projs), Some((false, 0)));
        // Raise the project above the experience lows: experience goes.
        let projs2 = {
            let mut p = entry_with("proj-high", (2023, None), (2023, None), false);
            p.kind = "project";
            vec![(0.50, p)]
        };
        assert_eq!(global_trim_victim(&exp, &projs2), Some((true, 1)));
        assert_eq!(global_trim_victim(&[], &[]), None);
    }
}
