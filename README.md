# resume

One-page LaTeX resume built from a pool of experiences/projects, tailored per job application.

Source of truth is `pool/` (seeded from `Resume_old.pdf`). Each experience/education/project is its own named-header file (`--title-- --company-- --dates-- --location-- --tags-- --bullets--`), see `pool/README.md`. Strictly validated: missing/duplicate/unknown fields, bare `--`, and content before the first header are hard errors.

Custom minimal one-page template: `template.tex` (pdflatex + tectonic compatible).

Rust builder (`src/`): `main.rs` orchestration, `pool.rs` parsing + LaTeX render, `decisions.rs` OpenRouter Decisions client, `pair.rs` + `ranking.rs` ported from `~/programming/muse-tricks` (bridging-first dealer + normalized random-walk ranking).

## quickstart

```bash
# needs a LaTeX engine: system texlive *or* user-space tectonic
#   tectonic static binary -> ~/.local/bin

# 1. drop a job description in jobs/, e.g. jobs/acme-backend.txt
# 2. set your OpenRouter key (env or .env; .env is gitignored):
#   OPENROUTER_KEY=...

# 3. build a pairwise-ranked one-pager:
cargo run --release -- --job jobs/example-backend.txt --out resume.pdf

# other useful flags:
cargo run --release -- --job jobs/foo.txt --out resume.pdf --max-exp 3
cargo run --release -- --job jobs/foo.txt --out resume.pdf --list-only   # preview ranking without compiling
cargo run --release -- --job jobs/foo.txt --out long.pdf --all           # longer version, everything, no AI, may be 2 pages
cargo run --release -- --job jobs/foo.txt --max-votes 16                 # cap pairwise decisions per kind (must stay connected)
cargo run --release -- --job jobs/foo.txt --model google/gemini-decision-1  # default
cargo test
```

## how selection works

- **Filtered by salience, ordered by kind.** Experience and projects are ranked in separate pairwise tournaments (dealer from `muse-tricks/src/pair.rs`, one `POST /api/alpha/decisions` `choice` per pair on `google/gemini-decision-1`, random walk from `muse-tricks/src/ranking.rs`). The top `--max-exp`/`--max-proj` by salience are selected; then **experience displays newest-first by start date** (formal `--dates--`: `YYYY[-MM]` / `START - END` / `… - now`, see `pool/README.md`) and **projects display in salience order**. Ranking prints salience score + display date so the two orders stay distinguishable. Any missing key, HTTP error, refusal, or malformed answer aborts the build — no local scoring, no other providers, no `--no-ai` fallback.
- **All bullets, drop entries to fit.** Every selected entry renders all of its bullets. After compiling, if `pdfinfo` reports >1 page, the oldest experience / least-salient project is dropped and the PDF is rebuilt until one page.
- `--all` skips the API and includes every experience (newest-first by date) and project (filename order — no salience without the API; may be 2 pages).

## adding content

Add a file to the right directory, e.g. `pool/experience/my-thing.txt`:

```
--title--
My Role
--company--
My Org
--dates--
2024 - 2025
--location--
Remote
--tags--
python, react, tags-for-matching
--bullets--
Did a thing that matters
With numbers: 50% faster, 4,500 users
```

Headers may be in any order; `--location--` and `--tags--` are optional. One bullet per line in `--bullets--`. Rebuild against a job post to see it ranked.
