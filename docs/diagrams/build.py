#!/usr/bin/env python3
"""Builds the architecture, data-flow and data-inventory diagrams as SVG files.
Run: python3 build.py   (then render.sh for PNG and PDF)"""
import html

FONT = "-apple-system, 'Helvetica Neue', Helvetica, Arial, sans-serif"
INK, MUTED, LINE = "#111827", "#6b7280", "#d1d5db"
BUILT, WRITTEN, PLANNED = "#16a34a", "#d97706", "#9ca3af"
STATUS_LABEL = {BUILT: "Built and tested", WRITTEN: "Written, not deployed", PLANNED: "Not built yet"}

# One colour per kind of user data, used in the data-flow diagram and its key.
DATA = {
    "question": ("#2563eb", "Question text"),
    "passages": ("#059669", "Passages of file text"),
    "files": ("#ea580c", "File contents (passing through, not kept)"),
    "tokens": ("#dc2626", "Sign-in and access tokens"),
    "perms": ("#7c3aed", "File names and who may read them"),
    "audit": ("#6b7280", "Audit records (who, what, counts only)"),
}


def esc(t):
    return html.escape(t, quote=True)


def wrap(text, width, size):
    """Greedy word wrap using an average character width."""
    per = size * 0.54
    lines, cur = [], ""
    for word in text.split():
        trial = (cur + " " + word).strip()
        if len(trial) * per <= width or not cur:
            cur = trial
        else:
            lines.append(cur)
            cur = word
    if cur:
        lines.append(cur)
    return lines


class Svg:
    def __init__(self, w, h):
        self.w, self.h, self.out = w, h, []

    def add(self, s):
        self.out.append(s)

    def text(self, x, y, s, size=12, weight=400, fill=INK, anchor="start", italic=False):
        style = ' font-style="italic"' if italic else ""
        self.add(f'<text x="{x}" y="{y}" font-size="{size}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}"{style}>{esc(s)}</text>')

    def para(self, x, y, s, width, size=11, fill=MUTED, lead=1.35, weight=400, anchor="start"):
        lines = wrap(s, width, size)
        for i, line in enumerate(lines):
            self.text(x, y + i * size * lead, line, size=size, fill=fill, weight=weight, anchor=anchor)
        return len(lines) * size * lead

    def rect(self, x, y, w, h, fill="#fff", stroke=LINE, sw=1, rx=10, dash=None, opacity=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        o = f' fill-opacity="{opacity}"' if opacity else ""
        self.add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{fill}" stroke="{stroke}" stroke-width="{sw}"{d}{o}/>')

    def line(self, pts, color="#374151", sw=1.8, dash=None, arrow=True, both=False):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        pts_s = " ".join(f"{x},{y}" for x, y in pts)
        ms = ' marker-start="url(#arrow-start)"' if both else ""
        me = ' marker-end="url(#arrow)"' if arrow else ""
        self.add(f'<polyline points="{pts_s}" fill="none" stroke="{color}" stroke-width="{sw}"{d}{me}{ms} stroke-linejoin="round"/>')

    def badge(self, x, y, n, fill="#111827"):
        self.add(f'<circle cx="{x}" cy="{y}" r="11" fill="{fill}"/>')
        self.text(x, y + 4, str(n), size=12, weight=700, fill="#fff", anchor="middle")

    def dot(self, x, y, color, r=5):
        self.add(f'<circle cx="{x}" cy="{y}" r="{r}" fill="{color}"/>')

    def render(self):
        defs = (
            '<defs>'
            '<marker id="arrow" markerUnits="userSpaceOnUse" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="10" markerHeight="10" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="#374151"/></marker>'
            '<marker id="arrow-start" markerUnits="userSpaceOnUse" viewBox="0 0 10 10" refX="1" refY="5" markerWidth="10" markerHeight="10" orient="auto-start-reverse"><path d="M10,0 L0,5 L10,10 z" fill="#374151"/></marker>'
            '</defs>'
        )
        body = "\n".join(self.out)
        return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" height="{self.h}" viewBox="0 0 {self.w} {self.h}" font-family="{FONT}">'
                f'{defs}<rect width="{self.w}" height="{self.h}" fill="#ffffff"/>{body}</svg>')


def header(s, title, subtitle):
    s.text(40, 48, title, size=26, weight=700)
    s.para(40, 72, subtitle, s.w - 80, size=13)


def footer(s, y):
    s.text(40, y, "Status as of 2 November 2026. Nothing is deployed to AWS yet; the cloud side runs on a developer machine for testing.", size=11, fill=MUTED)


def component(s, x, y, w, h, title, sub, status, fill="#fff"):
    dash = "5 4" if status == PLANNED else None
    s.rect(x, y, w, h, fill=fill, stroke=status, sw=1.6, dash=dash)
    s.dot(x + 16, y + 20, status)
    s.text(x + 28, y + 24, title, size=13, weight=700)
    s.para(x + 14, y + 42, sub, w - 28, size=11)


def status_key(s, x, y):
    s.text(x, y, "Status", size=11, weight=700, fill=MUTED)
    cx = x + 52
    for color in (BUILT, WRITTEN, PLANNED):
        s.dot(cx, y - 4, color)
        s.text(cx + 10, y, STATUS_LABEL[color], size=11, fill=INK)
        cx += 20 + len(STATUS_LABEL[color]) * 6.2 + 24


# ------------------------------------------------------------------ 1. Architecture

def architecture():
    W, CH = 1640, 1262
    s = Svg(W, CH)
    header(s, "Private AI Workspace: architecture",
           "Where each part runs, what it does, and how far along it is. Your local files stay on your device. The firm cloud holds only text copied from sources the firm connects.")
    status_key(s, 40, 112)

    top, fh = 135, 860
    zones = [
        (30, 480, "YOUR DEVICE", "#eff6ff", "#93c5fd"),
        (560, 650, "FIRM CLOUD  (your AWS account)", "#f5f3ff", "#c4b5fd"),
        (1260, 350, "OUTSIDE SERVICES", "#f9fafb", "#d1d5db"),
    ]
    for x, w, label, fill, stroke in zones:
        s.rect(x, top, w, fh, fill=fill, stroke=stroke, sw=1.4, rx=16)
        s.text(x + 18, top + 26, label, size=12, weight=700, fill=MUTED)

    row = lambda i: top + 44 + i * 120
    H = 84
    # Device
    dx, dw = 60, 420
    dev = [
        ("Desktop app", "Ask, search, Sources and Connectors screens. Shows which passages are local and which are cloud.", BUILT),
        ("Cloud client and merge", "Asks the firm cloud for matching passages, merges them with local results and picks the best.", BUILT),
        ("Local search index", "Encrypted database of your local files' text. Never uploaded anywhere.", BUILT),
        ("File reader (separate process)", "Reads PDFs, Word, Excel and scans on this device. Scans use the operating system's OCR.", BUILT),
        ("Your folders and files", "Stay where they are. The app only reads the folders you approve.", BUILT),
        ("Operating-system credential store", "Holds the index encryption key and the sign-in tokens (Keychain or Credential Manager).", BUILT),
        ("Ask Claude", "Sends your question and the chosen passages straight to Bedrock. Uses your own AWS login for now.", BUILT),
    ]
    for i, (t, sub, st) in enumerate(dev):
        component(s, dx, row(i), dw, H, t, sub, st, fill="#fff")

    # Cloud
    c1, c2, cw, cfull = 590, 905, 285, 590
    cloud = [
        (c1, row(0), cfull, "API", "Search, connectors list, connect, sync, disconnect. Checks every request's sign-in and firm.", BUILT),
        (c1, row(1), cfull, "Database (PostgreSQL)", "Synced text, file names and who may read each file. Firms are kept apart by row-level security inside the database. Planned on RDS.", BUILT),
        (c1, row(2), cw, "File reader (restricted process)", "Reads PDF, Word, Excel and PowerPoint files. Empty environment, time and memory limits.", BUILT),
        (c2, row(2), cw, "Sync worker", "Pulls files and permissions from Google and Microsoft every few minutes. Resumes after a crash.", BUILT),
        (c1, row(3), cw, "Sign-in (Cognito)", "Required two-step sign-in. The firm ID is set by us and users cannot change it.", WRITTEN),
        (c2, row(3), cw, "Secret store for tokens", "Holds each firm's long-lived Google and Microsoft access. Developer machines use a private file instead.", PLANNED),
        (c1, row(4), cw, "Encryption keys (KMS)", "One key for the platform, one for audit logs, one per firm for files and secrets.", WRITTEN),
        (c2, row(4), cw, "File storage (S3)", "Created but not used yet. Original files are not kept.", WRITTEN),
        (c1, row(5), cfull, "Audit and alerts", "CloudTrail records every file access, GuardDuty watches for threats, alerts go to email.", WRITTEN),
        (c1, row(6), cfull, "Claude access broker", "Will give each app short-lived permission to call Claude directly, so firms need no AWS login of their own.", PLANNED),
    ]
    for x, y, w, t, sub, st in cloud:
        component(s, x, y, w, H, t, sub, st, fill="#fff")

    # Outside
    ox, ow = 1290, 290
    out = [
        (row(0), "Google and Microsoft sign-in", "Where an administrator approves read-only access. Passwords go only to them.", BUILT),
        (row(2), "Google Drive", "Read-only access to files and sharing settings.", BUILT),
        (row(3), "Microsoft 365", "OneDrive and SharePoint through Microsoft Graph. Read-only.", BUILT),
        (row(6), "Amazon Bedrock: Claude", "Answers questions. Zero data retention must be on before anything is sent.", BUILT),
    ]
    for y, t, sub, st in out:
        component(s, ox, y, ow, H, t, sub, st, fill="#fff")

    # Arrows. Numbered; the key below explains each one.
    mid = lambda i: row(i) + H / 2
    # 1 cloud client -> API (question text, passages back)
    s.line([(dx + dw, mid(1) - 10), (540, mid(1) - 10), (540, mid(0)), (c1, mid(0))], both=True)
    s.badge(540, mid(0) + 40, 1)
    # 2 cloud client -> sign-in
    s.line([(dx + dw, mid(1) + 12), (562, mid(1) + 12), (562, mid(3)), (c1, mid(3))], both=True)
    s.badge(562, mid(3) - 40, 2)
    # 3 app -> Claude (bottom channel)
    s.line([(dx + dw / 2, row(6) + H), (dx + dw / 2, 1030), (ox + ow / 2, 1030), (ox + ow / 2, row(6) + H)])
    s.badge(760, 1030, 3)
    # 4 API <-> provider sign-in
    s.line([(ox, mid(0)), (c1 + cfull, mid(0))], both=True)
    s.badge(1245, mid(0) - 14, 4)
    # 5 API <-> database
    s.line([(c1 + 140, row(0) + H), (c1 + 140, row(1))], both=True)
    s.badge(c1 + 140, row(0) + H + 18, 5)
    # 6 worker <-> database
    s.line([(c2 + 140, row(2)), (c2 + 140, row(1) + H)], both=True)
    s.badge(c2 + 140, row(1) + H + 18, 6)
    # 7 worker -> reader
    s.line([(c2, mid(2)), (c1 + cw, mid(2))], both=True)
    s.badge((c2 + c1 + cw) / 2, mid(2) - 16, 7)
    # 8 worker -> secrets
    s.line([(c2 + 140, row(2) + H), (c2 + 140, row(3))], both=True)
    s.badge(c2 + 140, row(2) + H + 18, 8)
    # 9 worker -> Google / Microsoft
    s.line([(c2 + cw, mid(2)), (ox, mid(2))], both=True)
    s.badge(1240, mid(2) - 14, 9)
    s.line([(c2 + cw, mid(2) + 14), (1240, mid(2) + 14), (1240, mid(3)), (ox, mid(3))], both=True)
    # 10 API -> secret store (the long-lived token is saved at connect time), along the right edge
    s.line([(c1 + cfull, row(0) + 62), (1200, row(0) + 62), (1200, mid(3)), (c2 + cw, mid(3))], color="#7c3aed", dash="6 4")
    s.badge(1200, row(1) + H / 2, 10, fill="#7c3aed")

    # Key
    ky = 1078
    s.rect(30, ky - 24, W - 60, 156, fill="#fafafa", stroke=LINE, rx=12)
    s.text(48, ky, "What the numbered arrows carry", size=12, weight=700)
    items = [
        "Your question text goes to the firm cloud to be searched. Matching passages come back. Nothing from your local files goes up.",
        "Sign-in to the firm's account. The firm and your role come from the sign-in token, never from the app.",
        "The question and the best passages (local and cloud) go straight to Claude on Bedrock. The firm cloud never sees this step.",
        "An administrator approves access at Google or Microsoft. A one-time code comes back to the API, which swaps it for the long-lived token.",
        "The API reads and writes synced text and permissions. Every query is limited to one firm by the database itself.",
        "The worker stores passages, names and permissions. It never stores the original files.",
        "Each downloaded file is handed to a locked-down reader process and discarded; only extracted text comes back.",
        "The worker fetches the firm's access token to read from Google and Microsoft.",
        "Read-only file listing, sharing settings and downloads from the connected sources.",
        "When a source is connected, the API saves its long-lived token in the secret store. The database never holds a token.",
    ]
    col_w = (W - 120) / 3
    for i, t in enumerate(items):
        cx = 48 + (i % 3) * col_w
        cy = ky + 24 + (i // 3) * 30
        s.badge(cx + 11, cy - 4, i + 1)
        s.para(cx + 28, cy, t, col_w - 44, size=10.5, fill="#374151", lead=1.25)
    footer(s, CH - 14)
    return s


# ------------------------------------------------------------------ 2. Data flow

def dataflow():
    W = 1640
    cols = ["You", "Desktop app", "Firm API", "Sync worker", "File reader", "Database and secrets", "Google / Microsoft", "Claude (Bedrock)"]
    x0, dx = 100, 198
    X = [x0 + i * dx for i in range(len(cols))]
    LINE_H = 13.5

    def label_width(a, b):
        span = abs(X[b] - X[a])
        return span - 40 if span >= 400 else 400

    def height(spec):
        if spec[0] == "self":
            return 66
        _, n, a, b, label, kind = spec[:6]
        return len(wrap(label, label_width(a, b), 11)) * LINE_H + 36

    # Each step: ("step", number, from, to, label, kind[, both]) or ("self", number, at, label, kind)
    flow_a = [
        ("step", 1, 0, 1, "Press Connect on Google Drive or Microsoft 365 (firm administrators only).", "perms"),
        ("step", 2, 1, 2, "Ask the API to start. The sign-in token proves who you are and which firm.", "tokens"),
        ("step", 3, 2, 1, "A signed sign-in link for the provider, valid for ten minutes. The app opens it in your browser.", "tokens"),
        ("step", 4, 0, 6, "You approve read-only access on Google's or Microsoft's own page. Your password goes only to them.", "tokens"),
        ("step", 5, 6, 2, "A one-time code returns to the API, which swaps it for a long-lived access token.", "tokens"),
        ("step", 6, 2, 5, "The token goes to the secret store. The connection row in the database holds no token.", "tokens"),
        ("step", 7, 3, 6, "Every few minutes the worker lists changes: file names and who may read each file.", "perms", True),
        ("step", 8, 3, 6, "It downloads each new or changed file (up to 50 MB).", "files", True),
        ("step", 9, 3, 4, "The reader extracts text in a locked-down process, and the file is then discarded.", "files", True),
        ("step", 10, 3, 5, "Passages, file names and permissions are stored. The original files are not.", "passages"),
    ]
    flow_b = [
        ("step", 1, 0, 1, "You type a question.", "question"),
        ("self", 2, 1, "Search your local index. Stays on this device.", "passages"),
        ("step", 3, 1, 2, "The question text goes to the firm cloud over HTTPS. Local files do not.", "question"),
        ("step", 4, 2, 5, "Search the database, limited to the files you may read in Google or Microsoft.", "question"),
        ("step", 5, 5, 1, "Matching passages and file names come back. The server logs only counts, never your question.", "passages"),
        ("self", 6, 1, "Merge local and cloud results. Keep the best ~8.", "passages"),
        ("step", 7, 1, 7, "The question plus the chosen passages go straight to Claude. Zero data retention must be on first.", "passages"),
        ("step", 8, 7, 1, "The answer comes back.", "passages"),
        ("step", 9, 1, 0, "You see the answer with citations and a privacy receipt: what was sent, to whom, and how many passages.", "audit"),
    ]
    head, pad = 78, 40
    heights = [sum(height(sp) for sp in f) + head + pad for f in (flow_a, flow_b)]
    top_a = 100
    top_b = top_a + heights[0] + 56
    key_y = top_b + heights[1] + 60
    H = int(key_y + 150)
    s = Svg(W, H)
    header(s, "Private AI Workspace: how your data moves",
           "Two journeys: connecting a source and syncing it, then asking a question. Arrow colour shows what kind of data is moving. Local files never reach the firm cloud.")

    def lane_bands(top, bottom):
        zones = [(0, 1, "YOUR DEVICE", "#eff6ff"), (2, 5, "FIRM CLOUD (AWS)", "#f5f3ff"), (6, 7, "OUTSIDE SERVICES", "#f9fafb")]
        for a_, b_, label, fill in zones:
            left, right = X[a_] - dx / 2 + 4, X[b_] + dx / 2 - 4
            s.rect(left, top, right - left, bottom - top, fill=fill, stroke="none", rx=14)
            s.text(left + 12, top + 18, label, size=10.5, weight=700, fill=MUTED)
        for i, name in enumerate(cols):
            s.text(X[i], top + 44, name, size=12, weight=700, anchor="middle")
            s.line([(X[i], top + 54), (X[i], bottom - 12)], color="#cbd5e1", sw=1.2, dash="3 4", arrow=False)

    def draw_step(y_top, spec):
        _, n, a, b, label, kind = spec[:6]
        both = len(spec) > 6 and spec[6]
        color = DATA[kind][0]
        lines = wrap(label, label_width(a, b), 11)
        y = y_top + len(lines) * LINE_H + 14
        s.line([(X[a], y), (X[b], y)], color=color, sw=2.4, both=both)
        left, right = min(X[a], X[b]), max(X[a], X[b])
        centred = right - left >= 400
        for i, line in enumerate(lines):
            ty = y - 10 - (len(lines) - 1 - i) * LINE_H
            if centred:
                s.text((left + right) / 2, ty, line, size=11, fill="#1f2937", anchor="middle")
            else:
                s.text(left + 10, ty, line, size=11, fill="#1f2937")
        s.badge(left - 17, y, n, fill=color)

    def draw_self(y_top, spec):
        _, n, a, label, kind = spec
        color = DATA[kind][0]
        x = X[a]
        s.rect(x + 10, y_top + 6, 250, 34, fill="#fff", stroke=color, sw=1.8, rx=8)
        for i, line in enumerate(wrap(label, 232, 10.5)[:2]):
            s.text(x + 20, y_top + 21 + i * 12.5, line, size=10.5, fill="#1f2937")
        s.badge(x - 17, y_top + 23, n, fill=color)

    def draw_flow(top, title, flow, bottom):
        s.text(40, top - 2, title, size=17, weight=700)
        lane_bands(top + 12, bottom)
        y = top + head
        for spec in flow:
            (draw_self if spec[0] == "self" else draw_step)(y, spec)
            y += height(spec)

    draw_flow(top_a, "A.  Connecting a source and syncing its files", flow_a, top_a + heights[0])
    draw_flow(top_b, "B.  Asking a question", flow_b, top_b + heights[1])

    ky = key_y
    s.rect(30, ky - 22, W - 60, 128, fill="#fafafa", stroke=LINE, rx=12)
    s.text(48, ky, "Kinds of data", size=12, weight=700)
    for i, (k, (c, name)) in enumerate(DATA.items()):
        cx = 48 + (i % 3) * 520
        cy = ky + 26 + (i // 3) * 24
        s.line([(cx, cy - 4), (cx + 34, cy - 4)], color=c, sw=3, arrow=False)
        s.text(cx + 44, cy, name, size=11.5)
    s.para(48, ky + 80, "Not shown: the firm cloud never calls Claude and never sees local passages. Today the app calls Claude with your own AWS login; a broker that hands out short-lived permission is planned. Audit records (who searched, how many results) are kept; question text is not.", W - 120, size=10.5, fill="#374151", lead=1.3)
    footer(s, H - 14)
    return s


# ------------------------------------------------------------------ 3. Data inventory

def inventory():
    rows = [
        ("Your local files", "On your device, where you keep them", "Not copied. Only the folders you approve are read.", BUILT),
        ("Text and index of your local files", "Encrypted database on your device. Key in the operating-system credential store.", "Never uploaded to the firm cloud. Deleted from Settings > Clear local workspace.", BUILT),
        ("Text of connected files (Google, Microsoft)", "Firm cloud database, as plain text columns, in a shared database separated per firm by row-level security. Disk encryption with an AWS key.", "Anyone with database access can read it. Deleted when the connector is removed; backups may keep it up to 14 days.", WRITTEN),
        ("Original connected files", "Not stored anywhere", "Downloaded, read for text, then discarded.", BUILT),
        ("Who may read each connected file", "Firm cloud database, as email, domain and group identities", "Refreshed with every sync and in full every six hours. Files whose permissions are unknown are hidden from everyone.", BUILT),
        ("Google and Microsoft access tokens", "Planned: AWS Secrets Manager. Developer machines: a private file readable only by you.", "Read-only access. Deleted when the connector is removed. A person can also withdraw it at Google or Microsoft.", PLANNED),
        ("Your sign-in", "Cognito (written, not deployed). The app keeps its tokens in the credential store.", "Two-step sign-in required. Tokens expire and refresh.", WRITTEN),
        ("Your question text", "Typed in the app. Sent to the firm cloud to search it, and to Claude with the chosen passages.", "The firm cloud does not store it (it logs only the length). The question history in the window is in memory and is lost when the app closes.", BUILT),
        ("Passages sent to Claude", "Amazon Bedrock", "Sent only after zero data retention is confirmed on the AWS account. The firm cloud does not see this step. The direct Claude API test mode has no such check and is for test documents only.", BUILT),
        ("Privacy receipts", "Your device (encrypted database)", "Each records what was sent, to whom, and how many passages came from local and cloud.", BUILT),
        ("Audit records", "Firm cloud database and AWS CloudTrail", "Who searched or changed a connection, and counts. Never question or file text.", WRITTEN),
    ]
    W = 1640
    colx = [40, 430, 960, 1560]  # last is right edge
    widths = [colx[1] - colx[0] - 24, colx[2] - colx[1] - 24, colx[3] - colx[2] - 24]
    size = 11.5
    # compute row heights
    heights = []
    for name, where, note, st in rows:
        lines = max(len(wrap(name, widths[0] - 20, size)), len(wrap(where, widths[1] - 20, size)), len(wrap(note, widths[2] - 20, size)))
        heights.append(lines * size * 1.35 + 22)
    H = int(150 + sum(heights) + 60)
    s = Svg(W, H)
    header(s, "Private AI Workspace: where your data lives",
           "Every kind of user data, where it is kept, who can read it and how it is removed. Coloured dots show how far along each part is.")
    status_key(s, 40, 112)
    y = 138
    s.rect(30, y, W - 60, 30, fill="#111827", stroke="none", rx=8)
    for i, label in enumerate(["Data", "Where it lives", "Who can read it, and how it is removed"]):
        s.text(colx[i] + 14, y + 20, label, size=12, weight=700, fill="#fff")
    y += 36
    for (name, where, note, st), h in zip(rows, heights):
        s.rect(30, y, W - 60, h - 4, fill="#fff", stroke=LINE, rx=8)
        s.dot(colx[0] + 6, y + 18, st)
        s.para(colx[0] + 20, y + 20, name, widths[0] - 20, size=size, fill=INK, weight=700, lead=1.35)
        s.para(colx[1] + 14, y + 20, where, widths[1] - 20, size=size, fill="#374151", lead=1.35)
        s.para(colx[2] + 14, y + 20, note, widths[2] - 20, size=size, fill="#374151", lead=1.35)
        y += h
    footer(s, H - 14)
    return s


if __name__ == "__main__":
    for name, fn in (("architecture", architecture), ("data-flow", dataflow), ("data-inventory", inventory)):
        svg = fn()
        open(f"{name}.svg", "w").write(svg.render())
        print(name, svg.w, svg.h)
