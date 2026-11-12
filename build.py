#!/usr/bin/env python3
"""Build a one-page LaTeX resume from pool/ selected for a job description.

Pool format: each file has fields delimited by a line containing exactly `--`.
See pool/README.md. Experience/project/education files: 6 fields
  role / org / dates / location / tags / bullets(one per line)

Usage:
  python3 build.py --job jobs/example-backend.txt --out resume.pdf
  python3 build.py --job jobs/foo.txt --out resume.pdf --max-exp 4 --max-proj 2 --max-bullets 3
  python3 build.py --job jobs/foo.txt --out resume.pdf --no-ai        # local keyword scoring only
  python3 build.py --job jobs/foo.txt --out resume.pdf --ai            # try LLM ranking, fall back to local
  python3 build.py --job jobs/foo.txt --out resume.pdf --all           # longer version, no filtering (may spill to 2pp)
  python3 build.py --job jobs/foo.txt --out resume.pdf --list-only    # print ranking, don't compile

AI mode: if --ai and ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_KEY /
OPENROUTER_API_KEY is set (also loaded from .env), asks the LLM
to score items 0-10 + pick best bullets. Otherwise uses local TF scoring
(tag overlap weighted 2x + bullet word overlap). Local mode is deterministic
and needs no network; AI mode tailors better to paraphrased job posts.
Set OPENROUTER_MODEL to override (default: openai/gpt-4o-mini).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).parent
POOL = ROOT / "pool"
TEMPLATE = ROOT / "template.tex"


def load_dotenv():
    """Load KEY=VALUE lines from ROOT/.env into os.environ (no override). Stdlib only."""
    env_path = ROOT / ".env"
    if not env_path.exists():
        return
    try:
        for line in env_path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            k, v = k.strip(), v.strip().strip('"').strip("'")
            if k and k not in os.environ:
                os.environ[k] = v
    except OSError:
        pass


load_dotenv()

STOPWORDS = set(
    """a an the and or of to in on for with as at by from is are was were be been
    we you they he she it this that these those our your their his her its
    will would can could should have has had do does did not no yes if then than
    so such very more most other some any all each both into out up down over
    under who whom whose what when where why how looking seeking join team""".split()
)

LATEX_SPECIAL = {
    "&": r"\&", "%": r"\%", "$": r"\$", "#": r"\#", "_": r"\_",
    "{": r"\{", "}": r"\}", "~": r"\textasciitilde{}",
    "^": r"\textasciircumflex{}",
}


def latex_escape(s: str) -> str:
    out = []
    for ch in s:
        if ch in LATEX_SPECIAL:
            out.append(LATEX_SPECIAL[ch])
        elif ch == "\\":
            out.append(r"\textbackslash{}")
        else:
            out.append(ch)
    return "".join(out)


def split_fields(text: str):
    """Split file content on lines that are exactly `--` (whitespace tolerant)."""
    parts = re.split(r"(?m)^\s*--\s*$", text.strip())
    return [p.strip() for p in parts]


def load_pool_file(path: Path):
    return split_fields(path.read_text(encoding="utf-8"))


def parse_entry(path: Path, kind: str):
    f = load_pool_file(path)
    # pad to 6 fields so hand-written files with missing location still parse
    while len(f) < 6:
        f.append("")
    role, org, dates, loc, tags_raw, bullets_raw = f[:6]
    tags = [t.strip().lower() for t in tags_raw.replace("\n", ",").split(",") if t.strip()]
    bullets = [b.strip() for b in bullets_raw.split("\n") if b.strip()]
    # strip leading "- "/"* " if author used markdown bullets
    bullets = [re.sub(r"^[-*•]\s*", "", b) for b in bullets]
    return {
        "id": path.stem,
        "kind": kind,
        "file": str(path.relative_to(ROOT)),
        "role": role,
        "org": org,
        "dates": dates,
        "location": loc,
        "tags": tags,
        "bullets": bullets,
    }


def load_pool():
    header_f = load_pool_file(POOL / "header.txt")
    while len(header_f) < 4:
        header_f.append("")
    name, phone, email, links_raw = header_f[:4]
    links = [l.strip() for l in links_raw.split("\n") if l.strip()]

    education = [parse_entry(p, "education") for p in sorted((POOL / "education").glob("*.txt"))]
    experience = [parse_entry(p, "experience") for p in sorted((POOL / "experience").glob("*.txt"))]
    proj_dir = POOL / "projects"
    projects = [parse_entry(p, "project") for p in sorted(proj_dir.glob("*.txt"))] if proj_dir.exists() else []
    skills_f = load_pool_file(POOL / "skills.txt")  # N fields, one category each

    return {
        "name": name, "phone": phone, "email": email, "links": links,
        "education": education, "experience": experience,
        "projects": projects, "skills": skills_f,
    }


def tokenize(text: str):
    toks = re.findall(r"[a-z0-9+#.]+", text.lower())
    return [t for t in toks if t not in STOPWORDS and len(t) > 1]


def local_score(job_tokens: set, entry) -> tuple[float, list[tuple[float, str]]]:
    """Score entry + each bullet by word overlap with job post. Tags count 2x."""
    job = job_tokens
    tag_text = " ".join(entry["tags"])
    tag_toks = set(tokenize(tag_text))
    base = len(tag_toks & job) * 2.0
    # full-text overlap (role+org+bullets) counts 1x
    full_toks = set(tokenize(entry["role"] + " " + entry["org"] + " " + " ".join(entry["bullets"])))
    base += len(full_toks & job) * 1.0
    # normalize slightly by length so huge entries don't always win
    base -= 0.05 * len(entry["bullets"])
    scored_bullets = []
    for b in entry["bullets"]:
        bt = set(tokenize(b))
        s = len(bt & job) + 0.5 * len(set(tokenize(" ".join(entry["tags"]))) & bt)
        scored_bullets.append((s, b))
    return base, scored_bullets


def llm_rank(job_text: str, entries) -> dict | None:
    """Ask an LLM to score entries. Returns {id: (score, bullet_indices)} or None."""
    anthropic_key = os.environ.get("ANTHROPIC_API_KEY")
    openai_key = os.environ.get("OPENAI_API_KEY")
    openrouter_key = os.environ.get("OPENROUTER_API_KEY") or os.environ.get("OPENROUTER_KEY")
    if not anthropic_key and not openai_key and not openrouter_key:
        return None

    items = [
        {"id": e["id"], "role": e["role"], "org": e["org"],
         "tags": e["tags"], "bullets": e["bullets"]}
        for e in entries
    ]
    prompt = (
        "You rank resume items against a job description. "
        "Return ONLY JSON: a list of {\"id\": str, \"score\": 0-10 float, "
        "\"bullets\": [indices most relevant, best first]}. "
        "Score 10 = directly relevant, 0 = irrelevant. Prefer specific "
        "skill/tool/domain overlap over generic seniority.\n\n"
        f"JOB DESCRIPTION:\n{job_text[:6000]}\n\nITEMS:\n{json.dumps(items)[:12000]}"
    )
    try:
        if anthropic_key:
            req = urllib.request.Request(
                "https://api.anthropic.com/v1/messages",
                data=json.dumps({
                    "model": os.environ.get("ANTHROPIC_MODEL", "claude-haiku-4-5"),
                    "max_tokens": 2000,
                    "messages": [{"role": "user", "content": prompt}],
                }).encode(),
                headers={
                    "Content-Type": "application/json",
                    "x-api-key": anthropic_key,
                    "anthropic-version": "2023-06-01",
                },
            )
            with urllib.request.urlopen(req, timeout=60) as r:
                body = json.load(r)
            text = "".join(b.get("text", "") for b in body.get("content", []))
        else:
            if openrouter_key:
                url = "https://openrouter.ai/api/v1/chat/completions"
                model = os.environ.get("OPENROUTER_MODEL", "openai/gpt-4o-mini")
                headers = {
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {openrouter_key}",
                }
                referer = os.environ.get("OPENROUTER_REFERER")
                if referer:
                    headers["HTTP-Referer"] = referer
                    headers["X-Title"] = os.environ.get("OPENROUTER_TITLE", "resume-builder")
            else:
                url = "https://api.openai.com/v1/chat/completions"
                model = os.environ.get("OPENAI_MODEL", "gpt-4o-mini")
                headers = {
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {openai_key}",
                }
            req = urllib.request.Request(
                url,
                data=json.dumps({
                    "model": model,
                    "messages": [{"role": "user", "content": prompt}],
                    "temperature": 0,
                }).encode(),
                headers=headers,
            )
            with urllib.request.urlopen(req, timeout=60) as r:
                body = json.load(r)
            text = body["choices"][0]["message"]["content"]
        m = re.search(r"\[.*\]", text, re.S)
        data = json.loads(m.group(0) if m else text)
        out = {}
        for d in data:
            out[d["id"]] = (float(d.get("score", 0)),
                            [int(i) for i in d.get("bullets", []) if isinstance(i, int)])
        return out
    except Exception as e:
        print(f"[warn] LLM ranking failed ({e}), falling back to local scoring.", file=sys.stderr)
        return None


def select(pool, job_text, max_exp, max_proj, max_bullets, use_ai):
    job_tokens = set(tokenize(job_text))
    entries = pool["experience"] + pool["projects"]
    llm = llm_rank(job_text, entries) if use_ai else None
    if llm:
        print("[info] using LLM ranking", file=sys.stderr)
    else:
        print("[info] using local keyword scoring", file=sys.stderr)

    ranked = []
    for e in entries:
        base, scored = local_score(job_tokens, e)
        if llm and e["id"] in llm:
            score, idxs = llm[e["id"]]
            # order bullets by LLM preference, then append the rest by local score
            ordered = []
            seen = set()
            for i in idxs:
                if 0 <= i < len(e["bullets"]) and i not in seen:
                    ordered.append(e["bullets"][i])
                    seen.add(i)
            rest = [b for _, b in sorted(scored, reverse=True) if b not in seen]
            bullets = (ordered + rest)[:max_bullets] if max_bullets else ordered + rest
        else:
            score = base
            bullets = [b for _, b in sorted(scored, reverse=True)][:max_bullets] if max_bullets else e["bullets"]
        ranked.append((score, e, bullets))

    exps = sorted([r for r in ranked if r[1]["kind"] == "experience"], key=lambda r: -r[0])
    projs = sorted([r for r in ranked if r[1]["kind"] == "project"], key=lambda r: -r[0])
    return exps[:max_exp], projs[:max_proj]


def render_entry(e, bullets) -> str:
    role, org, dates, loc = (latex_escape(e["role"]), latex_escape(e["org"]),
                             latex_escape(e["dates"]), latex_escape(e["location"]))
    s = f"\\entryHeading{{{role}}}{{{org}}}{{{dates}}}{{{loc}}}\n"
    if bullets:
        s += "\\begin{itemize}\n"
        for b in bullets:
            s += f"\\item {latex_escape(b)}\n"
        s += "\\end{itemize}\n"
    return s


def render_tex(pool, exps, projs) -> str:
    tpl = TEMPLATE.read_text(encoding="utf-8")
    parts = [pool["phone"], pool["email"]] + pool["links"]
    contact = " $\\vert$ ".join(latex_escape(p) for p in parts if p.strip())
    edu_tex = ""
    for e in pool["education"]:
        edu_tex += render_entry(e, e["bullets"])
    exp_tex = "".join(render_entry(e, b) for _, e, b in exps)
    proj_tex = ""
    if projs:
        proj_tex = "\\section*{Projects}\n" + "".join(render_entry(e, b) for _, e, b in projs)
    skills_tex = " \\\\\n".join(latex_escape(s) for s in pool["skills"] if s.strip())
    return (tpl.replace("@@NAME@@", latex_escape(pool["name"]))
               .replace("@@CONTACT@@", contact)
               .replace("@@EDUCATION@@", edu_tex)
               .replace("@@EXPERIENCE@@", exp_tex)
               .replace("@@PROJECTS@@", proj_tex)
               .replace("@@SKILLS@@", skills_tex))


def compile_tex(tex_text: str, out_pdf: Path, keep_tex: bool):
    """Returns True if 1 page, False if >1 page, None if unknown (no compiler/pdfinfo)."""
    build_dir = ROOT / ".build"
    build_dir.mkdir(exist_ok=True)
    tex_path = build_dir / "resume.tex"
    tex_path.write_text(tex_text, encoding="utf-8")
    if keep_tex:
        (ROOT / "resume.tex").write_text(tex_text, encoding="utf-8")
    has_pdflatex = shutil.which("pdflatex") is not None
    # also honour ~/.local/bin (tectonic installed there by default on this repo)
    for extra in [Path.home() / ".local" / "bin", Path.home() / ".cargo" / "bin"]:
        if (extra / "tectonic").exists():
            os.environ["PATH"] = str(extra) + os.pathsep + os.environ.get("PATH", "")
    has_tectonic = shutil.which("tectonic") is not None
    if not has_pdflatex and not has_tectonic:
        print("[error] no LaTeX engine found. Install texlive (`sudo pacman -S texlive-basic texlive-latexrecommended texlive-latexextra`) "
              "or tectonic (https://tectonic-typesetting.github.io) then re-run. .tex left at " + str(tex_path), file=sys.stderr)
        return None
    if has_tectonic:
        r = subprocess.run(["tectonic", str(tex_path), "--outdir", str(build_dir)],
                           capture_output=True, text=True)
        if r.returncode != 0:
            print(r.stdout[-3000:], file=sys.stderr)
            print(r.stderr[-3000:], file=sys.stderr)
            return False
    else:
        for _ in range(2):  # two passes for hyperref stability
            r = subprocess.run(["pdflatex", "-interaction=nonstopmode", "-halt-on-error",
                                "-output-directory", str(build_dir), str(tex_path)],
                               capture_output=True, text=True)
            if r.returncode != 0:
                print(r.stdout[-3000:], file=sys.stderr)
                print(r.stderr[-1000:], file=sys.stderr)
                return False
    pdf = build_dir / "resume.pdf"
    shutil.copy(pdf, out_pdf)
    print(f"[ok] wrote {out_pdf} ({out_pdf.stat().st_size // 1024} KB)")
    if keep_tex:
        (ROOT / "resume.tex").write_text(tex_text, encoding="utf-8")
    # page check
    try:
        info = subprocess.run(["pdfinfo", str(out_pdf)], capture_output=True, text=True)
        m = re.search(r"Pages:\s+(\d+)", info.stdout)
        if m:
            print(f"[info] pages: {m.group(1)}")
            return m.group(1) == "1"
        return True
    except FileNotFoundError:
        return True  # pdfinfo missing; assume ok


def main():
    ap = argparse.ArgumentParser(description="Build one-page resume for a job description")
    ap.add_argument("--job", required=True, help="path to job description .txt")
    ap.add_argument("--out", default="resume.pdf", help="output pdf path")
    ap.add_argument("--max-exp", type=int, default=4)
    ap.add_argument("--max-proj", type=int, default=2)
    ap.add_argument("--max-bullets", type=int, default=3)
    ap.add_argument("--ai", dest="ai", action="store_true", default=True)
    ap.add_argument("--no-ai", dest="ai", action="store_false")
    ap.add_argument("--all", action="store_true", help="include everything (longer version)")
    ap.add_argument("--list-only", action="store_true")
    ap.add_argument("--keep-tex", action="store_true")
    a = ap.parse_args()

    pool = load_pool()
    job_text = Path(a.job).read_text(encoding="utf-8")

    if a.all:
        exps = [(0, e, e["bullets"]) for e in pool["experience"]]
        projs = [(0, e, e["bullets"]) for e in pool["projects"]]
    else:
        exps, projs = select(pool, job_text, a.max_exp, a.max_proj, a.max_bullets, a.ai)

    print("=== selected experience ===")
    for s, e, b in exps:
        print(f"{s:5.1f}  {e['role']} @ {e['org']}  [{len(b)} bullets]")
    print("=== selected projects ===")
    for s, e, b in projs:
        print(f"{s:5.1f}  {e['role']} @ {e['org']}  [{len(b)} bullets]")
    if a.list_only:
        return

    # shrink-to-fit loop: drop lowest-ranked items until 1 page (skip for --all)
    tex = render_tex(pool, exps, projs)
    out = Path(a.out)
    pages = compile_tex(tex, out, a.keep_tex or a.all)
    if a.all:
        return
    while pages is False and (len(exps) > 1 or len(projs) > 0):
        if projs and (len(exps) <= 2 or len(projs) > 1):
            dropped = projs.pop()
        else:
            dropped = exps.pop()
        print(f"[info] >1 page, dropping lowest: {dropped[1]['id']} and rebuilding", file=sys.stderr)
        tex = render_tex(pool, exps, projs)
        pages = compile_tex(tex, out, a.keep_tex or a.all)
    if pages is False:
        print("[warn] still >1 page; try --max-exp 3 --max-bullets 2", file=sys.stderr)


if __name__ == "__main__":
    main()
