# resume

One-page LaTeX resume built from a pool of experiences/projects, tailored per job application.

Source of truth is `pool/` (seeded from `Resume_old.pdf`). Each experience/education/project is its own file, fields delimited by a line containing exactly `--`. See `pool/README.md`.

Custom minimal one-page template: `template.tex` (pdflatex + tectonic compatible).

## quickstart

```bash
# needs a LaTeX engine: system texlive *or* user-space tectonic
# tectonic bootstrap (no sudo): downloads static binary to ~/.local/bin
#   curl -sL "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%400.17.0/tectonic-0.17.0-x86_64-unknown-linux-gnu.tar.gz" | tar xz -C /tmp && cp /tmp/tectonic ~/.local/bin/

# 1. drop a job description in jobs/, e.g. jobs/acme-backend.txt
# 2. build a tailored one-pager:
python3 build.py --job jobs/example-backend.txt --out resume.pdf

# other useful flags:
python3 build.py --job jobs/foo.txt --out resume.pdf --no-ai          # local keyword scoring only (default is --ai with fallback)
python3 build.py --job jobs/foo.txt --out resume.pdf --max-exp 3 --max-bullets 2
python3 build.py --job jobs/foo.txt --out resume.pdf --list-only      # preview ranking without compiling
python3 build.py --job jobs/foo.txt --out long.pdf --all              # longer version, everything, may be 2 pages
```

## how selection works

- Local (default, no network): tokenizes the job post, scores each pool entry by tag overlap (2x) + body word overlap (1x), picks top `--max-exp` experiences + `--max-proj` projects, and top `--max-bullets` bullets per entry.
- AI (`--ai`, default on): if `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `OPENROUTER_KEY` / `OPENROUTER_API_KEY` is set (auto-loaded from `.env`, never commit it), asks the LLM to score 0-10 + order bullets by relevance; on any failure falls back to local. Set `ANTHROPIC_MODEL` / `OPENAI_MODEL` / `OPENROUTER_MODEL` to override (defaults: `claude-haiku-4-5` / `gpt-4o-mini` / `openai/gpt-4o-mini`).
- Shrink-to-fit: after compiling, if `pdfinfo` reports >1 page, drops the lowest-ranked item and rebuilds until 1 page.

Examples (both compile to 1 page letter):
- `jobs/example-backend.txt` → Red Planet Labs + Amazon + vizlab + agent-o-rama
- `jobs/example-frontend.txt` → vizlab + mge.tf + Microsoft + Red Planet Labs

## adding content

Add a file, e.g. `pool/experience/my-thing.txt`:

```
My Role
--
My Org
--
2024 -- 2025
--
Remote
--
python, react, tags-for-matching
--
Did a thing that matters
With numbers: 50% faster, 4,500 users
```

One bullet per line in the last field. Rebuild against a job post to see it ranked.
