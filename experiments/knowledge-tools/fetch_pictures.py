#!/usr/bin/env python3
"""Freeze the photos E9's attached-picture cases send, once: 500-px renditions of freely
licensed Wikimedia Commons files, with each one's author and licence written beside them. Never
a search engine's results (Brave's terms forbid benchmarking a model on them)."""

import json
import os
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures", "pictures")
UA = "goose-in-a-pond/0.1 (GIAP knowledge-tools bench; https://github.com/jarida-io/goose-in-a-pond)"
API = "https://commons.wikimedia.org/w/api.php"
# Licences that need only attribution, so a downscaled copy can sit in the repo.
FREE = ("CC BY 2.0", "CC BY 3.0", "CC BY 4.0", "CC0", "Public domain")


def get(url, raw=False):
    req = urllib.request.Request(url, headers={"user-agent": UA})
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = resp.read()
    return body if raw else json.loads(body)


def info(title):
    query = urllib.parse.urlencode({
        "action": "query", "titles": title, "prop": "imageinfo",
        "iiprop": "url|extmetadata", "iiurlwidth": 500, "format": "json",
    })
    page = next(iter(get(API + "?" + query)["query"]["pages"].values()))
    ii = page["imageinfo"][0]
    meta = ii.get("extmetadata", {})
    field = lambda k: (meta.get(k) or {}).get("value", "")
    return {
        "file": title,
        "page": ii["descriptionurl"],
        "thumb": ii["thumburl"],
        "license": field("LicenseShortName"),
        "license_url": field("LicenseUrl"),
        "artist": field("Artist"),
    }


def main():
    os.makedirs(OUT, exist_ok=True)
    cases = json.load(open(os.path.join(HERE, "cases-media.json")))["cases"]
    credits = []
    for case in cases:
        if not case.get("picture"):
            continue
        meta = info(case["picture"]["commons"])
        assert meta["license"] in FREE, "%s is %s" % (meta["file"], meta["license"])
        path = os.path.join(OUT, case["picture"]["path"])
        if not os.path.exists(path):
            with open(path, "wb") as f:
                f.write(get(meta["thumb"], raw=True))
        credits.append(dict(meta, path=case["picture"]["path"]))
        print("ok", case["id"], meta["file"], meta["license"])
    with open(os.path.join(OUT, "CREDITS.json"), "w") as f:
        json.dump(credits, f, indent=1, ensure_ascii=False)
        f.write("\n")


if __name__ == "__main__":
    main()
