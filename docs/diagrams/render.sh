#!/usr/bin/env bash
# Renders the SVG diagrams to PNG and one combined PDF with headless Chrome.
set -euo pipefail
cd "$(dirname "$0")"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
for name in architecture data-flow data-inventory; do
  read -r w h < <(python3 -c "import re,sys; s=open('$name.svg').read(); m=re.search(r'width=\"(\d+)\" height=\"(\d+)\"', s); print(m.group(1), m.group(2))")
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
    --window-size="$w,$h" --screenshot="$PWD/$name.png" "file://$PWD/$name.svg" >/dev/null 2>&1
done
python3 - <<'PY'
import re
pages = []
for n in ("architecture", "data-flow", "data-inventory"):
    s = open(f"{n}.svg").read()
    w, h = re.search(r'width="(\d+)" height="(\d+)"', s).groups()
    pages.append((n, int(w), int(h)))
css = "".join(f"@page p{i}{{size:{w}px {h}px;margin:0}} .p{i}{{page:p{i};break-after:page;width:{w}px;height:{h}px;overflow:hidden}} " for i, (n, w, h) in enumerate(pages))
body = "".join(f'<div class="p{i}"><img src="{n}.svg" width="{w}" height="{h}"></div>' for i, (n, w, h) in enumerate(pages))
open("_print.html", "w").write(f"<!doctype html><meta charset=utf-8><style>html,body{{margin:0}} img{{display:block}} {css}</style>{body}")
PY
"$CHROME" --headless=new --disable-gpu --no-pdf-header-footer --print-to-pdf="$PWD/private-ai-workspace-diagrams.pdf" "file://$PWD/_print.html" >/dev/null 2>&1
rm -f _print.html
ls -la *.svg *.png *.pdf
