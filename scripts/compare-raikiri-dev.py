#!/usr/bin/env python3
"""Compare development backends without updating production VRT goldens."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


def read_ppm(data):
    """Read binary P6 with comments; consume only the header delimiter."""
    offset = 0
    tokens = []
    while len(tokens) < 4:
        while offset < len(data) and data[offset] in b" \t\r\n":
            offset += 1
        if offset < len(data) and data[offset] == ord("#"):
            end = data.find(b"\n", offset)
            if end < 0:
                raise ValueError("unterminated PPM comment")
            offset = end + 1
            continue
        start = offset
        while offset < len(data) and data[offset] not in b" \t\r\n":
            offset += 1
        if start == offset:
            raise ValueError("incomplete PPM header")
        tokens.append(data[start:offset])
    if tokens[0] != b"P6" or tokens[3] != b"255":
        raise ValueError("expected 8-bit binary P6")
    width, height = map(int, tokens[1:3])
    if width <= 0 or height <= 0 or offset == len(data):
        raise ValueError("invalid PPM dimensions or delimiter")
    offset += 2 if data[offset:offset + 2] == b"\r\n" else 1
    pixels = data[offset:]
    if len(pixels) != width * height * 3:
        raise ValueError("PPM payload length does not match dimensions")
    return width, height, pixels


def difference(first, second):
    same = first[:2] == second[:2]
    different = maximum = None
    if same:
        deltas = [abs(a - b) for a, b in zip(first[2], second[2])]
        different = sum(any(deltas[i:i + 3]) for i in range(0, len(deltas), 3))
        maximum = max(deltas, default=0)
    return {"same_dimensions": same, "different_pixels": different,
            "maximum_channel_difference": maximum}


def run(command):
    return subprocess.run(command, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, env={**os.environ, "LC_ALL": "C"})


def version(program):
    result = run([program, "-v"])
    return (result.stdout + result.stderr).decode().splitlines()[0]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def measurements(stderr):
    elapsed = re.search(r"Elapsed \(wall clock\) time [^\n]*\):\s*(\S+)", stderr)
    rss = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
    if elapsed is None or rss is None:
        raise ValueError("GNU time did not report elapsed time and maximum RSS")
    parts = [float(part) for part in elapsed[1].split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return {"elapsed_seconds": seconds, "maximum_rss_kib": int(rss[1])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--font", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--measure", action="store_true", help="use GNU /usr/bin/time -v")
    parser.add_argument("fixtures", nargs="+", type=Path)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binary, font = args.binary.resolve(strict=True), args.font.resolve(strict=True)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    output = args.output_dir.resolve()
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"],
                                      cwd=Path(__file__).resolve().parent.parent).decode().strip()
    report = {"revision": revision, "binary_sha256": sha256(binary),
              "font_sha256": sha256(font), "font": str(font), "dpi": 96,
              "rasterizers": {name: version(name) for name in ["pdftoppm", "pdftocairo"]},
              "fixtures": []}
    for index, source in enumerate(args.fixtures):
        fixture = source.resolve(strict=True)
        entry = {"input": str(fixture), "sha256": sha256(fixture), "engines": {}}
        rasters = {}
        for engine in ["blitz", "raikiri"]:
            runs = []
            pages = []
            for repetition in range(args.runs):
                pdf = output / f"{index}-{fixture.stem}-{engine}-{repetition}.pdf"
                command = [str(binary), "render", str(fixture), "--engine", engine,
                           "--font", str(font), "--no-system-fonts", "--bookmarks",
                           "--creation-date", "2026-10-07T01:02:03Z", "-o", str(pdf)]
                measured = ["/usr/bin/time", "-v", *command] if args.measure else command
                result = run(measured)
                info = run(["pdfinfo", str(pdf)]).stdout.decode()
                page_count = int(re.search(r"^Pages:\s+(\d+)", info, re.MULTILINE)[1])
                record = {"command": measured, "pdf_sha256": sha256(pdf), "pages": page_count}
                if args.measure:
                    record.update(measurements(result.stderr.decode()))
                runs.append(record)
                if repetition == 0:
                    for page in range(1, page_count + 1):
                        prefix = output / f"{index}-{fixture.stem}-{engine}-page{page}"
                        run(["pdftoppm", "-r", "96", "-f", str(page), "-l", str(page),
                             "-singlefile", str(pdf), str(prefix)])
                        pages.append(read_ppm(Path(str(prefix) + ".ppm").read_bytes()))
                    text = run(["pdftotext", "-enc", "UTF-8", str(pdf), "-"]).stdout.decode()
            entry["engines"][engine] = {"runs": runs, "text": text,
                "dimensions": [[page[0], page[1]] for page in pages],
                "deterministic": len({record["pdf_sha256"] for record in runs}) == 1}
            rasters[engine] = pages
        entry["same_page_count"] = len(rasters["blitz"]) == len(rasters["raikiri"])
        entry["same_text"] = (entry["engines"]["blitz"]["text"].split()
                              == entry["engines"]["raikiri"]["text"].split())
        entry["page_differences"] = [difference(first, second) for first, second in
                                     zip(rasters["blitz"], rasters["raikiri"])]
        report["fixtures"].append(entry)
    (output / "comparison.json").write_text(json.dumps(report, indent=2) + "\n")
    print(output / "comparison.json")


if __name__ == "__main__":
    main()
