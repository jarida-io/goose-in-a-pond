#!/usr/bin/env python3
"""Fetch the image set named in data/images.json from Wikimedia Commons, re-checking each licence.

Usage: fetch_images.py OUT_DIR
Each image is saved as <id>.jpg at 640 px wide; a file whose licence is no longer CC0 or
public domain is refused rather than fetched.
"""

import io
import json
import sys
import urllib.parse
import urllib.request
from pathlib import Path

from PIL import Image

UA = {"User-Agent": "giap-p0-embed/0.1 (https://github.com/jarida-io/goose-in-a-pond)"}
OK = ("cc0", "public domain", "pd")


def info(title):
    q = {"action": "query", "format": "json", "titles": title, "prop": "imageinfo",
         "iiprop": "url|extmetadata", "iiurlwidth": "640"}
    url = "https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode(q)
    page = next(iter(json.load(urllib.request.urlopen(urllib.request.Request(url, headers=UA)))["query"]["pages"].values()))
    ii = page["imageinfo"][0]
    return ii["thumburl"], ii["extmetadata"].get("LicenseShortName", {}).get("value", "")


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((Path(__file__).resolve().parent / "data/images.json").read_text())
    for im in manifest["images"]:
        url, lic = info(im["file"])
        if not lic.lower().startswith(OK):
            print(f"refused {im['id']}: licence {lic!r}")
            continue
        raw = urllib.request.urlopen(urllib.request.Request(url, headers=UA)).read()
        img = Image.open(io.BytesIO(raw)).convert("RGB")
        img.thumbnail((640, 640))
        img.save(out / f"{im['id']}.jpg", quality=90)
        print(f"{im['id']} {img.size} {lic}")


if __name__ == "__main__":
    main()
