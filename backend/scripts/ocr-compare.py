#!/usr/bin/env python3
"""Compares OCR engines on handwritten or scanned pages: Tesseract (what the server uses today)
against Claude Haiku 5.5 and Claude Sonnet 5.5 reading the picture directly.

  python3 backend/scripts/ocr-compare.py FOLDER [--claude] [--ignore-case] [--out DIR]

FOLDER holds pictures (png, jpg, jpeg, tif, tiff, webp). Next to a picture, the correct text can be
given as NAME.txt (plain text) or NAME.json (the GNHK dataset's annotation format: a list of words
with "text", "polygon" and "line_idx"). Pictures without correct text are still transcribed so the
results can be read by eye. Tesseract runs for free and on this machine. --claude sends each
picture to the Claude API with the key the desktop app already uses, and costs real money (a few
tenths of a cent a page on Haiku, about a cent on Sonnet), so it is off unless asked for.

Scores follow the GNHK paper: character accuracy and word accuracy from edit distance (1 minus
distance over the length of the correct text, never below zero)."""

import argparse, base64, io, json, os, pathlib, re, subprocess, sys, time, urllib.request

EXTENSIONS = {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp"}
# Dollars per million tokens: (input, output). Haiku 5.5 is five times dearer above 100k prompt tokens.
PRICES = {"claude-haiku-5-5": (0.10, 0.50), "claude-sonnet-5-5": (2.00, 10.00)}
PROMPT = ("Transcribe all of the text in this image exactly as written, handwritten and printed, "
          "keeping the line breaks. Output only the transcription. Write [illegible] for a word you "
          "cannot read, and do not guess at or add anything that is not there.")
SPECIAL = {"%math%", "%SC%", "%NA%"}  # GNHK's markers for maths, scribbles and non-text


def edit_distance(a, b):
    """Levenshtein distance between two sequences (characters or words)."""
    previous = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        current = [i]
        for j, y in enumerate(b, 1):
            current.append(min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + (x != y)))
        previous = current
    return previous[-1]


def accuracy(truth, guess):
    return max(0.0, 1 - edit_distance(truth, guess) / len(truth)) if truth else float("nan")


def normalise(text, ignore_case):
    text = re.sub(r"\s+", " ", text).strip()
    return text.lower() if ignore_case else text


def truth_for(picture):
    plain = picture.with_suffix(".txt")
    if plain.exists():
        return plain.read_text(errors="replace")
    annotated = picture.with_suffix(".json")
    if annotated.exists():
        words = json.loads(annotated.read_text())
        words = [w for w in words if w.get("text") not in SPECIAL]
        lines = {}
        for w in words:
            lines.setdefault(w["line_idx"], []).append(w)
        ordered = []
        for index in sorted(lines):
            ordered.append(" ".join(w["text"] for w in sorted(lines[index], key=lambda w: w["polygon"]["x0"])))
        return "\n".join(ordered)
    return None


def tesseract(picture):
    started = time.time()
    out = subprocess.run(["tesseract", str(picture), "stdout", "-l", "eng", "--psm", "3"],
                         capture_output=True, text=True, timeout=300).stdout
    return out, time.time() - started, None


def api_key():
    if os.environ.get("ANTHROPIC_API_KEY"):
        return os.environ["ANTHROPIC_API_KEY"]
    path = pathlib.Path.home() / "Library/Application Support/PrivateAIConnectorWorkspace/anthropic-api-key"
    if path.exists():
        return path.read_text().strip()
    sys.exit("No Claude API key: set ANTHROPIC_API_KEY or save it where the desktop app keeps it.")


def picture_for_claude(picture):
    """Shrinks a picture to at most 1568 pixels on the long side (what the API keeps anyway)."""
    from PIL import Image
    image = Image.open(picture).convert("RGB")
    image.thumbnail((1568, 1568))
    buffer = io.BytesIO()
    image.save(buffer, "JPEG", quality=90)
    return base64.b64encode(buffer.getvalue()).decode()


def claude(picture, model, key):
    body = {"model": model, "max_tokens": 4000, "output_config": {"effort": "low"},
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg",
                                              "data": picture_for_claude(picture)}},
                {"type": "text", "text": PROMPT}]}]}
    request = urllib.request.Request(
        "https://api.anthropic.com/v1/messages", json.dumps(body).encode(),
        {"x-api-key": key, "anthropic-version": "2026-10-21", "content-type": "application/json"})
    started = time.time()
    reply = json.load(urllib.request.urlopen(request, timeout=300))
    text = "".join(b.get("text", "") for b in reply["content"] if b.get("type") == "text")
    usage = reply["usage"]
    price_in, price_out = PRICES[model]
    cost = (usage["input_tokens"] * price_in + usage["output_tokens"] * price_out) / 1e6
    return text, time.time() - started, cost


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("folder")
    parser.add_argument("--claude", action="store_true", help="also run Claude Haiku and Sonnet (costs money)")
    parser.add_argument("--ignore-case", action="store_true")
    parser.add_argument("--out", default=None, help="where to write each transcript (default: FOLDER/ocr-results)")
    args = parser.parse_args()

    folder = pathlib.Path(args.folder)
    pictures = sorted(p for p in folder.iterdir() if p.suffix.lower() in EXTENSIONS)
    if not pictures:
        sys.exit(f"No pictures found in {folder}")
    out = pathlib.Path(args.out) if args.out else folder / "ocr-results"
    out.mkdir(parents=True, exist_ok=True)
    engines = [("tesseract", lambda p: tesseract(p))]
    if args.claude:
        key = api_key()
        engines += [(m, (lambda model: lambda p: claude(p, model, key))(m)) for m in PRICES]

    totals = {name: {"chars": [], "words": [], "seconds": 0.0, "cost": 0.0} for name, _ in engines}
    print(f"{'picture':32} {'engine':20} {'chars':>7} {'words':>7} {'secs':>6} {'cost':>9}")
    for picture in pictures:
        truth = truth_for(picture)
        for name, run in engines:
            text, seconds, cost = run(picture)
            (out / f"{picture.stem}.{name}.txt").write_text(text)
            line = f"{picture.name[:32]:32} {name:20} "
            if truth:
                t, g = normalise(truth, args.ignore_case), normalise(text, args.ignore_case)
                car, war = accuracy(t, g), accuracy(t.split(), g.split())
                totals[name]["chars"].append(car)
                totals[name]["words"].append(war)
                line += f"{car:7.1%} {war:7.1%}"
            else:
                line += f"{'-':>7} {'-':>7}"
            totals[name]["seconds"] += seconds
            totals[name]["cost"] += cost or 0
            print(line + f" {seconds:6.1f} {('$%.4f' % cost) if cost is not None else 'free':>9}")
    print("\nAverages over pictures that have correct text:")
    for name, t in totals.items():
        if t["chars"]:
            mean = lambda xs: sum(xs) / len(xs)
            print(f"  {name:20} characters {mean(t['chars']):6.1%}   words {mean(t['words']):6.1%}   "
                  f"total ${t['cost']:.4f}   {t['seconds']:.0f}s")
    print(f"\nTranscripts are in {out}")


if __name__ == "__main__":
    main()
