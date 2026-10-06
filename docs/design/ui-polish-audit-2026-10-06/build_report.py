#!/usr/bin/env python3
"""Assemble the UI polish audit into one visual report.

Reads findings/*.md (format defined in README.md), embeds every referenced
before/after SVG mock and real render, and writes:

  report.html            standalone page (open locally or print to PDF)
  <artifact path>        same page without the document skeleton (--artifact)

Run with agent-python (needs markdown-it-py and Pillow):

  agent-python docs/design/ui-polish-audit-2026-10-06/build_report.py \
      [--artifact /path/to/artifact.html] [--pdf report.pdf]
"""

from __future__ import annotations

import argparse
import base64
import html
import io
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

from markdown_it import MarkdownIt
from PIL import Image

ROOT = Path(__file__).resolve().parent
MD = MarkdownIt("commonmark", {"html": False}).enable("table")

AREAS = [
    ("TL", "timeline", "Chat timeline", "Messages, tool calls, disclosures, streaming, markdown and code in chat"),
    ("CP", "composer", "Composer & popovers", "Composer, dock, queue, pickers, menus and chips"),
    ("SH", "shell", "App shell", "Sidebar, title bar, tabs, workbench panes, empty states, palette, history"),
    ("DS", "design-system", "Design system", "Tokens, type scale, spacing, radii, icons, motion; Settings"),
    ("SS", "surfaces", "Secondary surfaces", "Files, diffs and PRs, terminal, browser, viewer, toasts, usage"),
]
SEVERITIES = {
    "P0": "Broken or unreadable",
    "P1": "Clearly off-brand, seen daily",
    "P2": "Polish",
    "P3": "Nit",
}
FIELD_KEYS = [
    ("severity", "severity"),
    ("category", "category"),
    ("where", "where"),
    ("what", "wrong"),
    ("why", "why"),
    ("proposed", "fix"),
    ("fix", "fix"),
    ("before", "before"),
    ("after", "after"),
    ("effort", "effort"),
]
IMG_RE = re.compile(r"((?:mocks|shots)/[^\s`'\")\]]+?\.(?:svg|png|jpe?g))", re.I)
FINDING_RE = re.compile(r"^###\s+([A-Z]{2}-\d+[a-z]?)\s*[—–:-]+\s*(.+?)\s*$")
BULLET_RE = re.compile(r"^-\s+\*\*(.+?)\*\*\s*:?\s*(.*)$")

MAX_SHOT_WIDTH = 1600


@dataclass
class Finding:
    id: str
    title: str
    area: str
    fields: dict[str, str] = field(default_factory=dict)
    extra: str = ""

    @property
    def severity(self) -> str:
        m = re.search(r"P[0-3]", self.fields.get("severity", ""))
        return m.group(0) if m else "P2"

    @property
    def effort(self) -> str:
        m = re.search(r"\b(S|M|L)\b", self.fields.get("effort", ""))
        return m.group(1) if m else ""


@dataclass
class Area:
    prefix: str
    slug: str
    name: str
    blurb: str
    findings: list[Finding] = field(default_factory=list)
    notes: list[tuple[str, str]] = field(default_factory=list)  # (heading, markdown)
    present: bool = False


def field_key(label: str) -> str | None:
    norm = label.strip().lower().replace("’", "'")
    for prefix, key in FIELD_KEYS:
        if norm.startswith(prefix):
            return key
    return None


def parse_finding(area: str, head: re.Match, body: list[str]) -> Finding:
    f = Finding(id=head.group(1), title=head.group(2).strip(), area=area)
    current: str | None = None
    buf: list[str] = []
    extra: list[str] = []

    def flush() -> None:
        if current is not None:
            f.fields[current] = "\n".join(buf).strip()

    for line in body:
        m = BULLET_RE.match(line)
        key = field_key(m.group(1)) if m else None
        if m and key:
            flush()
            current, buf = key, [m.group(2)]
        elif current is not None:
            buf.append(line[2:] if line.startswith("  ") else line)
        else:
            extra.append(line)
    flush()
    f.extra = "\n".join(extra).strip()
    return f


def parse_area(area: Area) -> None:
    path = ROOT / "findings" / f"{area.slug}.md"
    if not path.exists():
        return
    area.present = True
    lines = path.read_text(encoding="utf-8").splitlines()
    section_head: str | None = None
    section: list[str] = []
    finding_head: re.Match | None = None

    def close() -> None:
        nonlocal section, section_head, finding_head
        if finding_head is not None:
            area.findings.append(parse_finding(area.prefix, finding_head, section))
        elif section_head is not None or any(s.strip() for s in section):
            text = "\n".join(section).strip()
            if text:
                area.notes.append((section_head or "Overview", text))
        section, section_head, finding_head = [], None, None

    for line in lines:
        if line.startswith("# ") and not section_head and not finding_head and not area.notes:
            continue  # file title
        fm = FINDING_RE.match(line)
        if fm:
            close()
            finding_head = fm
        elif line.startswith("## "):
            close()
            section_head = line.lstrip("#").strip()
        else:
            section.append(line)
    close()
    order = {"P0": 0, "P1": 1, "P2": 2, "P3": 3}
    area.findings.sort(key=lambda f: order[f.severity])  # stable: keeps the file's own order within a severity


_img_cache: dict[str, str | None] = {}


def data_uri(rel: str) -> str | None:
    if rel in _img_cache:
        return _img_cache[rel]
    path = ROOT / rel
    uri: str | None = None
    if path.exists():
        suffix = path.suffix.lower()
        if suffix == ".svg":
            raw = path.read_bytes()
            uri = "data:image/svg+xml;base64," + base64.b64encode(raw).decode()
        else:
            img = Image.open(path)
            if img.width > MAX_SHOT_WIDTH:
                h = round(img.height * MAX_SHOT_WIDTH / img.width)
                img = img.resize((MAX_SHOT_WIDTH, h), Image.LANCZOS)
            out = io.BytesIO()
            if img.mode in ("RGBA", "LA", "P"):
                img.save(out, "PNG", optimize=True)
                mime = "image/png"
                if out.tell() > 450_000:
                    out = io.BytesIO()
                    bg = Image.new("RGB", img.size, (13, 13, 13))
                    bg.paste(img.convert("RGBA"), mask=img.convert("RGBA").split()[-1])
                    bg.save(out, "JPEG", quality=86, optimize=True)
                    mime = "image/jpeg"
            else:
                img.convert("RGB").save(out, "JPEG", quality=86, optimize=True)
                mime = "image/jpeg"
            uri = f"data:{mime};base64," + base64.b64encode(out.getvalue()).decode()
    _img_cache[rel] = uri
    return uri


def images_for(f: Finding, which: str) -> list[str]:
    found = [m.group(1) for m in IMG_RE.finditer(f.fields.get(which, ""))]
    conventional = f"mocks/{f.area}/{f.id}-{which}.svg"
    if (ROOT / conventional).exists() and conventional not in found:
        found.insert(0, conventional)
    seen: list[str] = []
    blobs: set[bytes] = set()
    for rel in found:
        path = ROOT / rel
        if rel in seen or not path.exists():
            continue
        blob = path.read_bytes()
        if blob in blobs:  # an alias such as "after" == "after-c"
            continue
        blobs.add(blob)
        seen.append(rel)
    return seen


def md_block(text: str) -> str:
    rendered = MD.render(text).strip()
    if rendered.count("<p>") == 1 and rendered.startswith("<p>") and rendered.endswith("</p>"):
        rendered = rendered[3:-4]
    return rendered


def figure(rel_list: list[str], kind: str, f: Finding) -> str:
    label = "Before" if kind == "before" else "After"
    if not rel_list:
        return (
            f'<figure class="shot {kind} missing"><figcaption><span class="dot"></span>{label}</figcaption>'
            f'<div class="stage empty">No mock supplied</div></figure>'
        )
    primary, rest = rel_list[0], rel_list[1:]
    uri = data_uri(primary)
    alt = html.escape(f"{label}: {f.id} {f.title}")
    def thumbs(rels: list[str], what: str) -> str:
        items = "".join(
            f'<button class="thumb" type="button" aria-label="Open {what} {html.escape(rel)}" title="{html.escape(rel)}">'
            f'<img loading="lazy" src="{data_uri(rel)}" alt="{what} {html.escape(rel)}"></button>'
            for rel in rels
            if data_uri(rel)
        )
        heading = "Real renders" if what == "real render" else ("Alternatives" if kind == "after" else "More mocks")
        return f'<div class="extras"><span>{heading}</span>{items}</div>' if items else ""

    extras = thumbs([r for r in rest if r.startswith("mocks/")], "mock") + thumbs(
        [r for r in rest if r.startswith("shots/")], "real render"
    )
    real = '<span class="tag">real render</span>' if primary.startswith("shots/") else ""
    return (
        f'<figure class="shot {kind}"><figcaption><span class="dot"></span>{label}{real}'
        f'<code>{html.escape(primary)}</code></figcaption>'
        f'<button class="stage" type="button" aria-label="Enlarge {alt}"><img loading="lazy" src="{uri}" alt="{alt}"></button>'
        + extras
        + "</figure>"
    )


def render_finding(f: Finding) -> str:
    sev = f.severity
    cats = [c.strip() for c in re.split(r"[·,]", f.fields.get("category", "")) if c.strip()]
    chips = "".join(f'<span class="chip">{html.escape(c)}</span>' for c in cats)
    effort = f'<span class="effort" title="Effort">Effort {f.effort}</span>' if f.effort else ""
    rows = []
    for key, label in (("wrong", "What's wrong"), ("why", "Why it hurts"), ("fix", "Proposed fix")):
        if f.fields.get(key):
            rows.append(f'<div class="row {key}"><h4>{label}</h4><div class="md">{md_block(f.fields[key])}</div></div>')
    if f.extra:
        rows.append(f'<div class="row extra"><div class="md">{MD.render(f.extra)}</div></div>')
    where = f.fields.get("where", "")
    search = html.escape(" ".join([f.id, f.title, *f.fields.values()]).lower(), quote=True)
    return f"""
<article class="finding" id="{f.id}" data-area="{f.area}" data-sev="{sev}" data-search="{search}">
  <header>
    <a class="fid" href="#{f.id}">{f.id}</a>
    <span class="sev sev-{sev}" title="{SEVERITIES[sev]}">{sev}</span>
    <h3>{html.escape(f.title)}</h3>
  </header>
  <div class="meta">{chips}{effort}</div>
  {f'<div class="where md">{md_block(where)}</div>' if where else ''}
  <div class="pair">{figure(images_for(f, "before"), "before", f)}{figure(images_for(f, "after"), "after", f)}</div>
  <div class="body">{''.join(rows)}</div>
</article>"""


def render_notes(area: Area) -> str:
    if not area.notes:
        return ""
    parts = []
    for i, (head, text) in enumerate(area.notes):
        is_overview = head.lower().startswith(("area overview", "overview"))
        is_top = head.lower().startswith("top")
        open_attr = " open" if (is_overview or is_top) else ""
        parts.append(
            f'<details class="note"{open_attr}><summary>{html.escape(head)}</summary>'
            f'<div class="md">{MD.render(text)}</div></details>'
        )
    return '<div class="notes">' + "".join(parts) + "</div>"


def summary_text() -> str:
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    m = re.search(r"^## Summary\s*\n(.*)", readme, re.S | re.M)
    if not m:
        return ""
    body = m.group(1).strip()
    if body.startswith("_Filled in"):
        return ""
    rendered = MD.render(body)
    return re.sub(r"\b((?:TL|CP|SH|DS|SS)-\d+)\b", r'<a href="#\1">\1</a>', rendered)


CSS = r"""
/* Layout: one reading column of finding cards, before/after mocks side by side, sticky filter bar. */
:root {
  --bg: #f4f6fa; --surface: #ffffff; --raised: #eef1f6; --line: #dfe4ec;
  --fg: #202631; --muted: #445064; --faint: #5d6a7e;
  --accent: #5b43e8; --accent-soft: #ece9fd;
  --stage: #0b0b0d; --stage-line: #26262b;
  --p0: #c62828; --p0-bg: #fde8e8; --p1: #b4530a; --p1-bg: #fdeedd;
  --p2: #8a6100; --p2-bg: #fbf3d2; --p3: #4d586a; --p3-bg: #e9edf3;
  --before: #e5484d; --after: #2f9e5b;
  --font: "Geist", -apple-system, "SF Pro Text", "Helvetica Neue", Helvetica, sans-serif;
  --mono: "Geist Mono", "SF Mono", Menlo, monospace;
  --r-card: 14px; --r-ctl: 8px;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --bg: #060606; --surface: #111113; --raised: #1a1a1d; --line: #2a2a2f;
  --fg: #e8e8ea; --muted: #a9a9ae; --faint: #8e8e94;
  --accent: #a094f8; --accent-soft: #221e3a;
  --stage: #0b0b0d; --stage-line: #2a2a2f;
  --p0: #ff8a80; --p0-bg: #3a1615; --p1: #ffb26b; --p1-bg: #3a2412;
  --p2: #f5d04a; --p2-bg: #33290c; --p3: #b9bec8; --p3-bg: #24262b;
  --before: #ff6369; --after: #3dd68c;
  color-scheme: dark;
} }
:root[data-theme="dark"] {
  --bg: #060606; --surface: #111113; --raised: #1a1a1d; --line: #2a2a2f;
  --fg: #e8e8ea; --muted: #a9a9ae; --faint: #8e8e94;
  --accent: #a094f8; --accent-soft: #221e3a;
  --stage: #0b0b0d; --stage-line: #2a2a2f;
  --p0: #ff8a80; --p0-bg: #3a1615; --p1: #ffb26b; --p1-bg: #3a2412;
  --p2: #f5d04a; --p2-bg: #33290c; --p3: #b9bec8; --p3-bg: #24262b;
  --before: #ff6369; --after: #3dd68c;
  color-scheme: dark;
}
* { box-sizing: border-box; }
body { background: var(--bg); color: var(--fg); font: 15px/1.55 var(--font); margin: 0; -webkit-font-smoothing: antialiased; }
.wrap { max-width: 1240px; margin: 0 auto; padding-inline: 20px; padding-block: 40px 96px; }
h1, h2, h3, h4 { text-wrap: balance; margin: 0; }
code, .mono { font-family: var(--mono); font-size: 0.86em; }
a { color: var(--accent); }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 6px; }

.masthead { display: grid; gap: 14px; margin-bottom: 28px; }
.eyebrow { font: 500 12px/1 var(--mono); letter-spacing: 0.08em; text-transform: uppercase; color: var(--faint); display: flex; gap: 14px; flex-wrap: wrap; }
.masthead h1 { font-size: clamp(30px, 4.4vw, 46px); font-weight: 600; letter-spacing: -0.025em; line-height: 1.05; }
.masthead p { color: var(--muted); max-width: 68ch; margin: 0; font-size: 16px; }

.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 12px; margin: 26px 0 8px; }
.stat { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r-card); padding: 14px 16px; display: grid; gap: 10px; text-decoration: none; color: inherit; }
.stat:hover { border-color: var(--accent); }
.stat .top { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; }
.stat .name { font-weight: 600; font-size: 14px; }
.stat .count { font: 600 22px/1 var(--font); font-variant-numeric: tabular-nums; }
.stat .bar { display: flex; height: 6px; border-radius: 99px; overflow: hidden; background: var(--raised); }
.stat .bar span { display: block; height: 100%; }
.stat .legend { display: flex; gap: 10px; font: 500 11.5px/1 var(--mono); color: var(--faint); font-variant-numeric: tabular-nums; flex-wrap: wrap; }
.b-P0 { background: var(--p0); } .b-P1 { background: var(--p1); } .b-P2 { background: var(--p2); } .b-P3 { background: var(--p3); }

.summary { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r-card); padding: 22px 24px; margin: 22px 0 8px; }
.summary h2 { font-size: 18px; margin-bottom: 8px; }
.summary .md { max-width: 80ch; }

.filters { position: sticky; top: env(safe-area-inset-top, 0px); z-index: 5; background: color-mix(in srgb, var(--bg) 86%, transparent); backdrop-filter: saturate(1.6) blur(18px); -webkit-backdrop-filter: saturate(1.6) blur(18px); border-bottom: 1px solid var(--line); margin: 26px -20px 0; padding: 12px 20px; display: flex; flex-wrap: wrap; gap: 10px; align-items: center; }
.seg { display: inline-flex; background: var(--raised); border-radius: 10px; padding: 3px; gap: 2px; flex-wrap: wrap; }
.seg button { font: 500 13px/1 var(--font); color: var(--muted); background: none; border: 0; padding: 7px 11px; border-radius: 7px; cursor: pointer; }
.seg button[aria-pressed="true"] { background: var(--surface); color: var(--fg); box-shadow: 0 1px 2px rgb(0 0 0 / 0.12), 0 0 0 0.5px rgb(0 0 0 / 0.06); }
.sevs { display: inline-flex; gap: 6px; flex-wrap: wrap; }
.sevs button { font: 600 12px/1 var(--mono); border: 1px solid var(--line); background: var(--surface); color: var(--muted); padding: 7px 10px; border-radius: 99px; cursor: pointer; opacity: 0.55; }
.sevs button[aria-pressed="true"] { opacity: 1; }
.sevs button[data-sev="P0"][aria-pressed="true"] { color: var(--p0); background: var(--p0-bg); border-color: transparent; }
.sevs button[data-sev="P1"][aria-pressed="true"] { color: var(--p1); background: var(--p1-bg); border-color: transparent; }
.sevs button[data-sev="P2"][aria-pressed="true"] { color: var(--p2); background: var(--p2-bg); border-color: transparent; }
.sevs button[data-sev="P3"][aria-pressed="true"] { color: var(--p3); background: var(--p3-bg); border-color: transparent; }
.filters input { flex: 1 1 200px; min-width: 0; font: 14px var(--font); color: var(--fg); background: var(--surface); border: 1px solid var(--line); border-radius: var(--r-ctl); padding: 8px 12px; }
.filters .shown { font: 500 12px/1 var(--mono); color: var(--faint); font-variant-numeric: tabular-nums; }

.area { margin-top: 48px; scroll-margin-top: 80px; }
.area > header { display: grid; gap: 4px; margin-bottom: 16px; }
.area > header h2 { font-size: 26px; letter-spacing: -0.015em; font-weight: 600; display: flex; gap: 12px; align-items: baseline; flex-wrap: wrap; }
.area > header h2 .px { font: 600 13px/1 var(--mono); color: var(--accent); background: var(--accent-soft); padding: 5px 8px; border-radius: 6px; }
.area > header p { color: var(--muted); margin: 0; }
.area .pending { color: var(--faint); font-style: italic; }

.notes { display: grid; gap: 8px; margin-bottom: 18px; }
.note { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 0 18px; }
.note summary { cursor: pointer; padding: 13px 0; font-weight: 600; font-size: 14px; list-style-position: inside; }
.note[open] summary { border-bottom: 1px solid var(--line); margin-bottom: 4px; }
.note .md { padding-bottom: 12px; overflow-x: auto; }

.finding { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r-card); padding: 20px 22px 22px; margin-top: 14px; display: grid; gap: 12px; scroll-margin-top: 80px; }
.finding:target { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft); }
.finding > header { display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
.finding h3 { font-size: 18px; font-weight: 600; letter-spacing: -0.01em; flex: 1 1 280px; min-width: 0; }
.fid { font: 600 13px/1 var(--mono); color: var(--faint); text-decoration: none; }
.fid:hover { color: var(--accent); }
.sev { font: 700 11.5px/1 var(--mono); padding: 4px 7px; border-radius: 6px; }
.sev-P0 { color: var(--p0); background: var(--p0-bg); } .sev-P1 { color: var(--p1); background: var(--p1-bg); }
.sev-P2 { color: var(--p2); background: var(--p2-bg); } .sev-P3 { color: var(--p3); background: var(--p3-bg); }
.meta { display: flex; gap: 6px; flex-wrap: wrap; }
.chip, .effort { font: 500 12px/1 var(--font); color: var(--muted); background: var(--raised); padding: 5px 9px; border-radius: 99px; }
.effort { font-family: var(--mono); }
.where { font-size: 13px; color: var(--muted); overflow-wrap: anywhere; }
.where code { color: var(--fg); }

.pair { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.shot { margin: 0; display: grid; gap: 8px; align-content: start; min-width: 0; }
.shot figcaption { display: flex; align-items: center; gap: 8px; font: 600 12px/1 var(--font); letter-spacing: 0.04em; text-transform: uppercase; min-width: 0; }
.shot figcaption code { margin-left: auto; text-transform: none; letter-spacing: 0; font-weight: 400; color: var(--faint); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; font-size: 11px; }
.shot .dot { width: 8px; height: 8px; border-radius: 50%; flex: none; }
.shot.before figcaption { color: var(--before); } .shot.before .dot { background: var(--before); }
.shot.after figcaption { color: var(--after); } .shot.after .dot { background: var(--after); }
.shot .tag { font: 500 10.5px/1 var(--mono); text-transform: none; letter-spacing: 0; color: var(--muted); background: var(--raised); padding: 3px 6px; border-radius: 5px; }
.stage { display: block; width: 100%; padding: 0; border: 1px solid var(--stage-line); border-radius: 12px; background: var(--stage); overflow: hidden; cursor: zoom-in; }
.stage img { display: block; width: 100%; height: auto; }
.shot.before .stage { box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--before) 28%, transparent); }
.shot.after .stage { box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--after) 28%, transparent); }
.stage.empty { display: grid; place-items: center; min-height: 120px; color: #8e8e94; font-size: 13px; cursor: default; }
.extras { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; font: 500 11.5px/1 var(--font); color: var(--faint); }
.thumb { padding: 0; border: 1px solid var(--line); border-radius: 8px; background: var(--stage); overflow: hidden; width: 120px; cursor: zoom-in; }
.thumb img { display: block; width: 100%; height: 72px; object-fit: cover; object-position: top left; }

.body { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 18px; padding-top: 4px; }
.row h4 { font: 600 11.5px/1 var(--font); letter-spacing: 0.06em; text-transform: uppercase; color: var(--faint); margin-bottom: 7px; }
.row.fix h4 { color: var(--after); }
.row.extra { grid-column: 1 / -1; }
.md { font-size: 14px; color: var(--fg); min-width: 0; overflow-wrap: anywhere; }
.md p { margin: 0 0 8px; } .md p:last-child { margin-bottom: 0; }
.md ul, .md ol { margin: 4px 0 8px; padding-left: 20px; }
.md li { margin: 2px 0; }
.md code { background: var(--raised); padding: 1px 5px; border-radius: 5px; overflow-wrap: anywhere; }
.md pre { background: var(--raised); padding: 10px 12px; border-radius: 8px; overflow-x: auto; font-size: 12.5px; }
.md pre code { background: none; padding: 0; overflow-wrap: normal; }
.md table { border-collapse: collapse; font-size: 13px; margin: 8px 0; font-variant-numeric: tabular-nums; }
.md th, .md td { border-bottom: 1px solid var(--line); padding: 6px 10px; text-align: left; vertical-align: top; }
.md th { color: var(--muted); font-weight: 600; }

dialog.lightbox { border: 0; padding: 0; background: transparent; max-width: min(1400px, 96vw); max-height: 94vh; }
dialog.lightbox::backdrop { background: rgb(0 0 0 / 0.72); backdrop-filter: blur(6px); }
dialog.lightbox img { display: block; max-width: 100%; max-height: 90vh; border-radius: 12px; background: var(--stage); }
dialog.lightbox button { position: absolute; top: 10px; right: 10px; font: 600 13px var(--font); border: 0; border-radius: 99px; padding: 8px 12px; background: rgb(255 255 255 / 0.9); color: #111; cursor: pointer; }

@media (max-width: 860px) {
  .pair { grid-template-columns: 1fr; }
  .body { grid-template-columns: 1fr; gap: 12px; }
  .finding { padding: 16px; }
}
@media (prefers-reduced-motion: no-preference) {
  .finding, .stat, .seg button { transition: border-color 160ms ease, box-shadow 200ms ease, background-color 160ms ease; }
}
@page { size: A4 landscape; margin: 10mm; }
@media print {
  .filters, dialog { display: none !important; }
  body { font-size: 11px; }
  .wrap { padding-block: 0; padding-inline: 0; max-width: none; }
  .pair { grid-template-columns: 1fr 1fr; }
  .body { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .pair, .row, .finding > header, .note { break-inside: avoid; }
  .finding { break-inside: auto; padding: 14px 16px; }
  .area { break-before: page; }
}
"""

JS = r"""
(() => {
  const cards = [...document.querySelectorAll('.finding')];
  const areaBtns = [...document.querySelectorAll('.seg button')];
  const sevBtns = [...document.querySelectorAll('.sevs button')];
  const q = document.getElementById('q');
  const shown = document.querySelector('.shown');
  let area = 'all';
  const sevs = new Set(['P0', 'P1', 'P2', 'P3']);
  function apply() {
    const term = (q.value || '').trim().toLowerCase();
    let n = 0;
    for (const c of cards) {
      const ok = (area === 'all' || c.dataset.area === area) && sevs.has(c.dataset.sev) && (!term || c.dataset.search.includes(term));
      c.hidden = !ok; if (ok) n++;
    }
    document.querySelectorAll('.area').forEach(s => { s.hidden = area !== 'all' && s.dataset.area !== area; });
    shown.textContent = n + ' of ' + cards.length + ' shown';
  }
  areaBtns.forEach(b => b.addEventListener('click', () => {
    area = b.dataset.area; areaBtns.forEach(x => x.setAttribute('aria-pressed', String(x === b))); apply();
  }));
  sevBtns.forEach(b => b.addEventListener('click', () => {
    const s = b.dataset.sev; if (sevs.has(s)) sevs.delete(s); else sevs.add(s);
    b.setAttribute('aria-pressed', String(sevs.has(s))); apply();
  }));
  q.addEventListener('input', apply);
  const dlg = document.querySelector('dialog.lightbox');
  const big = dlg.querySelector('img');
  document.addEventListener('click', e => {
    const t = e.target.closest('.stage:not(.empty), .thumb');
    if (!t) return;
    const img = t.querySelector('img'); big.src = img.src; big.alt = img.alt; dlg.showModal();
  });
  dlg.addEventListener('click', e => { if (e.target === dlg || e.target.tagName === 'BUTTON') dlg.close(); });
  apply();
})();
"""


def build(standalone: bool) -> str:
    areas = [Area(*a) for a in AREAS]
    for a in areas:
        parse_area(a)
    total = sum(len(a.findings) for a in areas)
    sev_total = {s: sum(1 for a in areas for f in a.findings if f.severity == s) for s in SEVERITIES}

    stats = []
    for a in areas:
        counts = {s: sum(1 for f in a.findings if f.severity == s) for s in SEVERITIES}
        n = max(len(a.findings), 1)
        bar = "".join(f'<span class="b-{s}" style="width:{counts[s] / n * 100:.2f}%"></span>' for s in SEVERITIES if counts[s])
        legend = "".join(f"<span>{s} {counts[s]}</span>" for s in SEVERITIES)
        stats.append(
            f'<a class="stat" href="#area-{a.prefix}"><div class="top"><span class="name">{a.name}</span>'
            f'<span class="count">{len(a.findings)}</span></div><div class="bar">{bar}</div>'
            f'<div class="legend">{legend}</div></a>'
        )

    sections = []
    for a in areas:
        if not a.present:
            inner = '<p class="pending">Audit still running.</p>'
        else:
            inner = render_notes(a) + "".join(render_finding(f) for f in a.findings)
        sections.append(
            f'<section class="area" id="area-{a.prefix}" data-area="{a.prefix}"><header>'
            f'<h2><span class="px">{a.prefix}</span>{a.name}</h2><p>{a.blurb}</p></header>{inner}</section>'
        )

    summary = summary_text()
    summary_html = f'<section class="summary"><h2>Cross-cutting summary</h2><div class="md">{summary}</div></section>' if summary else ""
    sev_line = " · ".join(f"{sev_total[s]} {s}" for s in SEVERITIES)
    area_btns = '<button type="button" data-area="all" aria-pressed="true">All</button>' + "".join(
        f'<button type="button" data-area="{a.prefix}" aria-pressed="false">{a.name}</button>' for a in areas
    )
    sev_btns = "".join(
        f'<button type="button" data-sev="{s}" aria-pressed="true" title="{d}">{s}</button>' for s, d in SEVERITIES.items()
    )

    body = f"""<title>LUNA UI Polish Audit</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700&family=Geist+Mono:wght@400;500;600;700&display=swap">
<style>{CSS}</style>
<div class="wrap">
  <header class="masthead">
    <div class="eyebrow"><span>Audit · 2026-10-06</span><span>Baseline 590cf4b6</span><span>macOS · GPUI</span></div>
    <h1>LUNA UI Polish Audit</h1>
    <p>Every place the interface falls short of an Apple-grade finish: sizing, alignment, readability, icons and motion, with the chat timeline under the closest look. Each finding shows the issue as it renders today and the proposed fix with exact values.</p>
  </header>
  <div class="eyebrow">{total} findings · {sev_line}</div>
  <div class="stats">{''.join(stats)}</div>
  {summary_html}
  <nav class="filters" aria-label="Filter findings">
    <div class="seg" role="group" aria-label="Area">{area_btns}</div>
    <div class="sevs" role="group" aria-label="Severity">{sev_btns}</div>
    <input id="q" type="search" placeholder="Search findings, files, values…" aria-label="Search findings">
    <span class="shown"></span>
  </nav>
  {''.join(sections)}
</div>
<dialog class="lightbox"><button type="button">Close</button><img alt=""></dialog>
<script>{JS}</script>
"""
    if standalone:
        return (
            '<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
            '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
            + body.replace("<div class=\"wrap\">", "</head><body><div class=\"wrap\">", 1)
            + "</body></html>\n"
        )
    return body


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--artifact", type=Path, help="also write the skeleton-less artifact page here")
    ap.add_argument("--pdf", type=Path, help="also print the standalone page to this PDF with headless Chrome")
    args = ap.parse_args()

    out = ROOT / "report.html"
    page = build(standalone=True)
    out.write_text(page, encoding="utf-8")
    print(f"report.html: {len(page.encode()) / 1e6:.2f} MB")
    if args.artifact:
        art = build(standalone=False)
        args.artifact.write_text(art, encoding="utf-8")
        print(f"artifact: {len(art.encode()) / 1e6:.2f} MB -> {args.artifact}")
    if args.pdf:
        chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        cmd = [
            chrome, "--headless=new", "--disable-gpu", "--no-pdf-header-footer",
            "--virtual-time-budget=15000", f"--print-to-pdf={args.pdf.resolve()}", out.resolve().as_uri(),
        ]
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        if res.returncode != 0:
            print(res.stderr[-2000:], file=sys.stderr)
            return res.returncode
        print(f"pdf: {args.pdf.stat().st_size / 1e6:.2f} MB -> {args.pdf}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
