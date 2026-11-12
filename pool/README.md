# pool format

Every experience / project / education is its own named-header file.
Headers are lines of the form `--name--` (lowercase name between two
`--`). Fields may appear in any order, but each header may appear only
once. There is no default and no fallback: a missing, duplicate, or
unknown field, content before the first header, or a bare `--` line is
a hard error naming the file and line.

## experience / project file

Required: `--title--`, `--company--`, `--dates--`, `--bullets--`.
Optional: `--location--`, `--tags--` (may be omitted or left blank).

```
--title--
Junior Developer
--company--
Red Planet Labs
--dates--
2024 - 2026-08
--location--
--tags--
clojure, rama, react, frontend, api, jvm, agents, langgraph, langsmith, llm, debugging, developer platform
--bullets--
Developed features on Rama's developer platform (Clojure)
Built entire API server + React frontend for agent-o-rama, a LangSmith/LangGraph competitor for the JVM
```

`--tags--` is a comma-separated list. `--bullets--` is one bullet per
line (a leading `- `, `* `, or `• ` is stripped). All bullets render;
fit is achieved by dropping whole entries (oldest experience first,
least-salient project first).

### `--dates--` is formalized

Only these shapes are accepted; free-text dates are a hard error:

```
YYYY                     2022
YYYY-MM                  2025-06
YYYY - YYYY[-MM]         2018 - 2019, 2024 - 2026-08
YYYY - now               2023 - now
```

Years `1900`–`2100`, months `01`–`12`, separator exactly `" - "`, end
never before start. `Entry::dates` renders pretty (`2024 - Aug 2026`,
`Sep 2019 - Dec 2023`, `2023 - now`) while the parsed range drives
ordering: experience is filtered by salience, then displayed
newest-first by start date (year-only starts sort as the earliest of
the year); projects are displayed in salience order and their dates
are validated but never used for ordering.

Example: `experience/red-planet-labs.txt`.

## education file

Same shape, but `--school--` / `--degree--` in place of
`--title--` / `--company--`:

```
--school--
Khoury College of Computer Sciences at Northeastern University
--degree--
B.S. in Computer Science
--dates--
2019-09 - 2023-12
--location--
Boston, MA
--tags--
computer science, algorithms, programming languages, compilers, networks, data visualization
--bullets--
Relevant coursework: Programming Languages, Compilers, Object Oriented Design, Algorithms, Computer Aided Reasoning, Software Development, Networks, Data Visualization
```

## header.txt

Required: `--name--`, `--email--`. Optional: `--phone--`, `--links--`
(each link one per line):

```
--name--
Tommy Morriss
--phone--
(206) 305-4633
--email--
thmorriss@gmail.com
--links--
GitHub: tommy-mor
LinkedIn: tommy-mor
```

## skills.txt

One named field per skill category; the header is the label and is
rendered title-cased (`--os-cloud--` → `Os Cloud`). Any names are
allowed, order is preserved, each value must be non-empty:

```
--languages--
Languages: Strong: Python (NumPy, Matplotlib, Pandas, Qt), JavaScript (React, React Native, Angular, Vue.js), Java, Lua, Clojure | Intermediate: Haskell, OCaml, PHP, Arduino | Beginner: Rust
--os-cloud--
Operating Systems / Cloud: Linux (10 years), iOS, Android, Windows (Azure), AWS
--interests--
Interests: Long-distance backpacking, competitive video games, biking, alternative keyboard layouts/input methods
```

Parser: `src/pool.rs` (`parse_fields`, `parse_experience`,
`parse_education`, `parse_header`, `parse_skills`).
