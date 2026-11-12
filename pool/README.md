# pool format

Every experience / education / project is its own file.
Fields are delimited by a line containing exactly `--`.

## experience / project file (6 fields)

```
Role / Title
--
Organization
--
Dates (e.g. 2024--Aug 2026, or 2 Months 2022)
--
Location (may be empty)
--
tags, comma-separated, lowercase (used for matching against job descriptions)
--
bullet one
bullet two
bullet three (one bullet per line)
```

Example: see `experience/red-planet-labs.txt`.

## education file (6 fields, same layout)

```
School
--
Degree
--
Dates
--
Location
--
tags
--
detail line one
detail line two (one per line)
```

## header.txt (4 fields)

```
Name
--
Phone
--
Email
--
link one
link two (one per line, e.g. `GitHub: tommy-mor`)
```

## skills.txt (N fields, one skill category per field)

```
Languages: Strong: ... | Intermediate: ... | Beginner: ...
--
Operating Systems: ...
--
Interests: ...
```

Parser: split on lines matching `^\s*--\s*$`, strip whitespace. See `build.py:load_pool_file`.
