//! resume — build a one-page LaTeX resume from the pool, ranked by
//! pairwise OpenRouter decision-model judgments.
//!
//! Flow: parse the named-header pool (`pool/`) → deal pairs with the
//! muse-tricks dealer (`pair.rs`) → judge each pair with
//! `google/gemini-decision-1` on OpenRouter (`decisions.rs`, the only
//! AI path) → settle the vote graph with the muse-tricks random walk
//! (`ranking.rs`) → filter top `--max-exp` experience by salience and
//! `--max-proj` projects by salience, then order experience
//! newest-first by start date (projects keep salience order) → render
//! `template.tex` with all bullets of the selected entries → compile
//! with tectonic/pdflatex → drop oldest (`experience`) / least-salient
//! (`projects`) entries until the PDF is one page.
//!
//! There is no local scoring and no other provider: without
//! `OPENROUTER_KEY` the build fails before compiling anything.

mod decisions;
mod pair;
mod pool;
mod ranking;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use decisions::DecisionsClient;
use pool::{describe, render_tex, Entry, Pool};
use ranking::{rank_groups, MAX_ITERS, TOL};

const ROOT: &str = ".";
const DEFAULT_MODEL: &str = decisions::DEFAULT_MODEL;

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
    /// Max experience entries to include.
    #[arg(long, default_value_t = 4)]
    max_exp: usize,
    /// Max project entries to include.
    #[arg(long, default_value_t = 2)]
    max_proj: usize,
    /// OpenRouter Decisions model id. Always served by OpenRouter.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Cap on pairwise decisions per kind. Default is the full
    /// tournament `n*(n-1)/2`. Smaller values must still leave the vote
    /// graph connected or the build fails (no silent partial ranking).
    #[arg(long)]
    max_votes: Option<usize>,
    /// Include everything without AI ranking (may spill past one page).
    #[arg(long)]
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

/// Rank one kind (experience or projects) by sequential pairwise
/// decisions. Returns the entries best-first with their settled scores.
/// With fewer than two entries there is nothing to compare: the single
/// entry scores `1.0` with no API call.
fn rank_kind(
    kind: &str,
    entries: &[Entry],
    job_text: &str,
    client: &DecisionsClient,
    max_votes: Option<usize>,
) -> Result<Vec<(f64, Entry)>, String> {
    let n = entries.len();
    if n == 0 {
        return Ok(vec![]);
    }
    if n == 1 {
        return Ok(vec![(1.0, entries[0].clone())]);
    }
    let n32 = n as u32;
    let budget = max_votes.unwrap_or((n * (n - 1)) / 2);
    let texts: Vec<String> = entries.iter().map(describe).collect();

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
        let (pa_share, pb_share) =
            client.decide_pair(job_text, &texts[pa as usize], &texts[pb as usize])?;
        // Edges flow loser -> winner weighted by the winner's share
        // (muse-tricks orientation): presented-A's share lands on
        // (presented-B -> presented-A) and vice versa.
        edges.push((pb as usize, pa as usize, pa_share));
        edges.push((pa as usize, pb as usize, pb_share));
        let pair_key = canon(a, b);
        current = Some(pair_key);
        voted.insert(pair_key);
        eprintln!(
            "[vote {}/{}] ({kind}) {} vs {}: presented-a {:.2} presented-b {:.2}",
            round + 1,
            budget,
            entries[a as usize].id,
            entries[b as usize].id,
            pa_share,
            pb_share
        );
        round += 1;
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
    let ranked = groups[0]
        .ranked
        .iter()
        .map(|&(idx, score)| (score, entries[idx].clone()))
        .collect();
    Ok(ranked)
}

/// Order selected experience newest-first by start date (ties: later
/// end first, then `id` for determinism). Salience decides *which*
/// entries are selected; date decides *where they appear*.
fn sort_by_date_desc(items: &mut [(f64, Entry)]) {
    items.sort_by(|(_, a), (_, b)| {
        b.range
            .start_key()
            .cmp(&a.range.start_key())
            .then_with(|| b.range.end_key().cmp(&a.range.end_key()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Sort criterion shared by both selection paths. Wraps [`rank_kind`]
/// for the AI path; `--all` skips the API and returns every entry,
/// with experience ordered newest-first by date (projects keep file
/// order — there is no salience to order by without the API).
fn select(
    pool: &Pool,
    job_text: &str,
    args: &Args,
) -> Result<(Vec<(f64, Entry)>, Vec<(f64, Entry)>), String> {
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
        return Ok((exps, projs));
    }
    // One client per build; constructed lazily so `--all` needs no key.
    let client = DecisionsClient::from_env(&args.model)?;
    eprintln!("[info] ranking with {} via OpenRouter Decisions API", client.model());
    let exps = rank_kind("experience", &pool.experience, job_text, &client, args.max_votes)?;
    let projs = rank_kind("projects", &pool.projects, job_text, &client, args.max_votes)?;
    Ok((exps, projs))
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

    let (ranked_exps, ranked_projs) = select(&pool, &job_text, &args)?;
    let take_exps = if args.all { usize::MAX } else { args.max_exp };
    let take_projs = if args.all { usize::MAX } else { args.max_proj };
    // Filter by salience first (take), then order experience
    // newest-first by date. Projects keep salience order.
    let mut ordered_exps: Vec<(f64, Entry)> =
        ranked_exps.iter().take(take_exps).cloned().collect();
    sort_by_date_desc(&mut ordered_exps);
    let ordered_projs: Vec<(f64, Entry)> = ranked_projs.iter().take(take_projs).cloned().collect();

    println!("(experience: selected top {} by salience, displayed newest-first)", ordered_exps.len());
    print_selection("selected experience", &ordered_exps);
    print_selection("selected projects", &ordered_projs);
    if args.list_only {
        return Ok(());
    }

    // Shrink-to-fit: all bullets are kept, so fit by dropping whole
    // entries from the tail. Experience tails are the oldest, project
    // tails are the least salient. Policy: drop a project when one is
    // droppable and experience is already small.
    let mut exps: Vec<Entry> = ordered_exps.iter().map(|(_, e)| e.clone()).collect();
    let mut projs: Vec<Entry> = ordered_projs.iter().map(|(_, e)| e.clone()).collect();
    let mut tex = render_tex(&template, &pool, &exps, &projs);
    let mut pages_ok = compile_tex(&tex, &args.out, args.keep_tex || args.all, &root)?;
    if !args.all {
        while !pages_ok && (!exps.is_empty() || !projs.is_empty()) {
            if !projs.is_empty() && (exps.len() <= 2 || projs.len() > 1) {
                let dropped = projs.pop().unwrap();
                eprintln!("[info] >1 page, dropping least-salient project: {} and rebuilding", dropped.id);
            } else if !exps.is_empty() {
                let dropped = exps.pop().unwrap();
                eprintln!("[info] >1 page, dropping oldest experience: {} and rebuilding", dropped.id);
            } else {
                break;
            }
            tex = render_tex(&template, &pool, &exps, &projs);
            pages_ok = compile_tex(&tex, &args.out, args.keep_tex, &root)?;
        }
        if !pages_ok {
            eprintln!("[warn] still >1 page with {} experience + {} projects; try --max-exp 3", exps.len(), projs.len());
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
}
