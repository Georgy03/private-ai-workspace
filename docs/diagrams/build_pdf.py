#!/usr/bin/env python3
"""Builds docs/private-ai-workspace-architecture.pdf from architecture.md and the three SVG
diagrams, using headless Chrome to print. Run: python3 docs/diagrams/build_pdf.py"""
import html, os, re, subprocess, sys, datetime, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
DOCS = os.path.dirname(HERE)
CHROME = os.environ.get("CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
OUT = os.path.join(DOCS, "private-ai-workspace-architecture.pdf")
DIAGRAMS = [
    ("architecture", "System architecture"),
    ("data-flow", "How a question and its data flow"),
    ("data-inventory", "What data lives where"),
]


def inline(text):
    text = html.escape(text, quote=False)
    text = re.sub(r"`([^`]+)`", lambda m: f"<code>{m.group(1)}</code>", text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*([^*\s][^*]*)\*(?![\w*])", r"<em>\1</em>", text)
    return text


def slug(text):
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def convert(md):
    lines = md.split("\n")
    out, toc = [], []
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("```"):
            block = []
            i += 1
            while i < len(lines) and not lines[i].startswith("```"):
                block.append(lines[i])
                i += 1
            out.append("<pre>" + html.escape("\n".join(block)) + "</pre>")
            i += 1
            continue
        if line.startswith("# "):
            i += 1
            continue  # the cover page carries the title
        m = re.match(r"(#{2,3}) (.*)", line)
        if m:
            level, title = len(m.group(1)), m.group(2)
            ident = slug(title)
            if level == 2:
                toc.append((ident, title))
            out.append(f'<h{level} id="{ident}">{inline(title)}</h{level}>')
            i += 1
            continue
        if line.startswith("|"):
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                rows.append([c.strip() for c in lines[i].strip().strip("|").split("|")])
                i += 1
            head, body = rows[0], [r for r in rows[2:]]
            t = "<table><thead><tr>" + "".join(f"<th>{inline(c)}</th>" for c in head) + "</tr></thead><tbody>"
            for r in body:
                t += "<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>"
            out.append(t + "</tbody></table>")
            continue
        if re.match(r"\s*[-*] ", line):
            items = []
            while i < len(lines) and re.match(r"\s*[-*] ", lines[i]):
                items.append(re.sub(r"^\s*[-*] ", "", lines[i]))
                i += 1
            out.append("<ul>" + "".join(f"<li>{inline(x)}</li>" for x in items) + "</ul>")
            continue
        if re.match(r"\d+\. ", line):
            items = []
            while i < len(lines) and re.match(r"\d+\. ", lines[i]):
                items.append(re.sub(r"^\d+\. ", "", lines[i]))
                i += 1
            out.append("<ol>" + "".join(f"<li>{inline(x)}</li>" for x in items) + "</ol>")
            continue
        if line.startswith("Status"):
            i += 1
            continue  # shown on the cover page
        if line.strip():
            para = [line]
            i += 1
            while i < len(lines) and lines[i].strip() and not re.match(r"(#|\||```|\s*[-*] |\d+\. )", lines[i]):
                para.append(lines[i])
                i += 1
            out.append(f"<p>{inline(' '.join(para))}</p>")
            continue
        i += 1
    return "\n".join(out), toc


def svg_size(name):
    s = open(os.path.join(HERE, f"{name}.svg")).read()
    w, h = re.search(r'width="(\d+)" height="(\d+)"', s).groups()
    return int(w), int(h)


def main():
    md = open(os.path.join(DOCS, "architecture.md")).read()
    status = re.search(r"^Status[^\n]*", md, re.M).group(0)
    body, toc = convert(md)
    today = datetime.date.today().strftime("%B %-d, %Y")
    toc_html = "".join(f'<li><a href="#{i}">{html.escape(t)}</a></li>' for i, t in toc)
    pages_css, diagram_pages = "", ""
    for n, (name, title) in enumerate(DIAGRAMS):
        w, h = svg_size(name)
        pages_css += f"@page d{n} {{ size: {w}px {h + 70}px; margin: 0 }} .d{n} {{ page: d{n}; break-before: page; width: {w}px; height: {h + 70}px; overflow: hidden }}\n"
        diagram_pages += (
            f'<section class="d{n} diagram" id="{"figures" if n == 0 else f"figure-{n + 1}"}"><div class="dtitle">Figure {n + 1}. {html.escape(title)}</div>'
            f'<img src="{name}.svg" width="{w}" height="{h}"></section>'
        )
    css = f"""
@page {{ size: A4; margin: 18mm 16mm 18mm 16mm;
  @bottom-center {{ content: "Private AI Connector Workspace - architecture - page " counter(page); font: 8pt Helvetica, Arial, sans-serif; color: #777 }} }}
@page cover {{ size: A4; margin: 0; @bottom-center {{ content: "" }} }}
{pages_css}
html {{ font: 10pt/1.5 -apple-system, "Helvetica Neue", Helvetica, Arial, sans-serif; color: #1c211d }}
body {{ margin: 0 }}
.cover {{ page: cover; height: 297mm; padding: 70mm 22mm 0; box-sizing: border-box; background: #f6f7f4; break-after: page }}
.cover .kicker {{ font-size: 10pt; letter-spacing: .14em; text-transform: uppercase; color: #3f6b4a }}
.cover h1 {{ font: 700 30pt/1.15 Georgia, serif; margin: 10px 0 14px }}
.cover .sub {{ font-size: 13pt; color: #444; max-width: 140mm }}
.cover .meta {{ margin-top: 26mm; font-size: 9.5pt; color: #555; max-width: 150mm }}
.toc {{ break-after: page }}
.toc h2 {{ margin-top: 0 }}
.toc ol {{ list-style: none; padding: 0; margin: 0; columns: 1 }}
.toc li {{ padding: 4px 0; border-bottom: 1px solid #e4e7e1 }}
.toc a {{ color: #1c211d; text-decoration: none }}
h2 {{ font: 700 15pt Georgia, serif; margin: 22px 0 6px; padding-top: 6px; border-top: 2px solid #3f6b4a; break-after: avoid }}
h3 {{ font-size: 11pt; margin: 14px 0 4px; break-after: avoid }}
p {{ margin: 6px 0; orphans: 3; widows: 3 }}
ul, ol {{ margin: 6px 0 6px 18px; padding: 0 }} li {{ margin: 3px 0 }}
code {{ font: 8.6pt Menlo, Consolas, monospace; background: #eef0ea; padding: 0 3px; border-radius: 3px; overflow-wrap: anywhere }}
pre {{ font: 6.6pt/1.25 Menlo, Consolas, monospace; background: #f3f4f0; border: 1px solid #dde0d8; border-radius: 5px; padding: 8px 10px; overflow: hidden; white-space: pre; break-inside: avoid }}
table {{ width: 100%; border-collapse: collapse; margin: 8px 0; font-size: 8.8pt; break-inside: auto }}
th {{ text-align: left; background: #ebeee8; }}
th, td {{ border: 1px solid #d5d9d0; padding: 4px 6px; vertical-align: top; overflow-wrap: anywhere }}
tr {{ break-inside: avoid }}
.diagram {{ background: white }} .dtitle {{ font: 700 20px Georgia, serif; padding: 18px 28px 6px; height: 46px; box-sizing: border-box }}
.diagram img {{ display: block }}
"""
    page = f"""<!doctype html><meta charset="utf-8"><title>Private AI Connector Workspace - architecture</title><style>{css}</style>
<section class="cover"><div class="kicker">Architecture document</div>
<h1>Private AI Connector Workspace</h1>
<div class="sub">Multi-firm cloud index with a separate local index, Google Drive and Microsoft 365 connectors, and cited AI answers.</div>
<div class="meta">{html.escape(today)}<br><br>{inline(status)}</div></section>
<section class="toc"><h2>Contents</h2><ol>{toc_html}<li><a href="#figures">Figures: architecture, data flow, data inventory</a></li></ol></section>
{body}
{diagram_pages}"""
    with tempfile.TemporaryDirectory() as tmp:
        # The SVGs sit next to build.py, so the temporary page links to them by absolute path.
        page = page.replace('src="', f'src="file://{HERE}/')
        path = os.path.join(tmp, "doc.html")
        open(path, "w").write(page)
        subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--no-pdf-header-footer",
                        f"--print-to-pdf={OUT}", f"file://{path}"], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print(OUT, os.path.getsize(OUT))


if __name__ == "__main__":
    sys.exit(main())
