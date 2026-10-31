#!/usr/bin/env python3
"""Switch the Flatpak manifest between release and local-build form.

The local Flatpak build uses the root JSON manifest and the working tree as
its source. ``to-git`` is retained for release tooling that needs to point the
manifest at a tagged source.

``just release`` uses ``to-git`` to bump the tag in place before tagging, so
the change lands in the release commit.


Usage:
  flatpak-manifest.py to-git VERSION REPO_URL [MANIFEST]
"""

import argparse
import json

MANIFEST = "io.github.nalladev.cosmic-ext-notes.json"


def load(manifest: str) -> dict:
    with open(manifest, encoding="utf-8") as f:
        return json.load(f)


def write(manifest: str, data: dict) -> None:
    with open(manifest, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)


    to_git = sub.add_parser("to-git", help="point the manifest at a release tag (in place)")
    to_git.add_argument("version")
    to_git.add_argument("repo_url")
    to_git.add_argument("manifest", nargs="?", default=MANIFEST)

    args = parser.parse_args()

    version = args.version.lstrip("v")
    data = load(args.manifest)
    data["modules"][0]["sources"][0] = {
        "type": "git",
        "url": args.repo_url,
        "tag": "v" + version,
    }
    write(args.manifest, data)
    print(f"{args.manifest}: source -> git tag v{version}")


if __name__ == "__main__":
    main()
