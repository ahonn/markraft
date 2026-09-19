#!/usr/bin/env python3
"""Generate a Sparkle appcast for a signed universal macOS release archive."""

import argparse
import base64
import binascii
from datetime import datetime, timezone
from email.utils import format_datetime
import os
from pathlib import Path
import re
import tempfile
from urllib.parse import quote
import xml.etree.ElementTree as ET


SPARKLE_NAMESPACE = "http://www.andymatuschak.org/xml-namespaces/sparkle"
ET.register_namespace("sparkle", SPARKLE_NAMESPACE)


def sparkle(name):
    return f"{{{SPARKLE_NAMESPACE}}}{name}"


def generate_appcast(archive, version, repository, signature, minimum_system_version="13.0.0"):
    """Validate release metadata and return a complete UTF-8 RSS document."""
    if not re.fullmatch(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", version, re.ASCII):
        raise ValueError("version must be a stable x.y.z version without leading zeroes")
    if not re.fullmatch(
        r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?/[A-Za-z0-9_.-]+",
        repository,
    ) or repository.split("/")[-1] in {".", ".."}:
        raise ValueError("repository must be a GitHub owner/repository name")
    if not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", minimum_system_version, re.ASCII):
        raise ValueError("minimum system version must be a numeric macOS version")
    try:
        decoded_signature = base64.b64decode(signature, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ValueError("signature must be a base64-encoded Ed25519 signature") from error
    if len(decoded_signature) != 64:
        raise ValueError("signature must encode exactly 64 bytes")
    if base64.b64encode(decoded_signature).decode("ascii") != signature:
        raise ValueError("signature must use canonical base64 encoding")

    archive = Path(archive)
    if archive.suffix.lower() != ".zip" or not archive.is_file():
        raise ValueError("archive must be an existing ZIP file")
    archive_size = archive.stat().st_size
    if archive_size == 0:
        raise ValueError("archive must not be empty")

    repository_url = f"https://github.com/{repository}"
    release_url = f"{repository_url}/releases/tag/v{version}"
    archive_url = f"{repository_url}/releases/download/v{version}/{quote(archive.name, safe='')}"

    rss = ET.Element("rss", version="2.0")
    channel = ET.SubElement(rss, "channel")
    ET.SubElement(channel, "title").text = "Markraft Updates"
    ET.SubElement(channel, "link").text = repository_url
    ET.SubElement(channel, "description").text = "Markraft macOS releases"
    ET.SubElement(channel, "language").text = "en"
    item = ET.SubElement(channel, "item")
    ET.SubElement(item, "title").text = f"Markraft {version}"
    ET.SubElement(item, "link").text = release_url
    ET.SubElement(item, sparkle("fullReleaseNotesLink")).text = release_url
    ET.SubElement(item, "pubDate").text = format_datetime(datetime.now(timezone.utc), usegmt=True)
    ET.SubElement(item, sparkle("version")).text = version
    ET.SubElement(item, sparkle("shortVersionString")).text = version
    ET.SubElement(item, sparkle("minimumSystemVersion")).text = minimum_system_version
    ET.SubElement(
        item,
        "enclosure",
        {
            "url": archive_url,
            "length": str(archive_size),
            "type": "application/octet-stream",
            sparkle("edSignature"): signature,
        },
    )
    ET.indent(rss, space="  ")
    return ET.tostring(rss, encoding="utf-8", xml_declaration=True) + b"\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--signature", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--minimum-system-version", default="13.0.0")
    args = parser.parse_args(argv)

    temporary_path = None
    try:
        if args.archive.resolve() == args.output.resolve():
            raise ValueError("output must not overwrite the release archive")
        document = generate_appcast(
            args.archive, args.version, args.repository, args.signature, args.minimum_system_version
        )
        # Keep an existing feed intact if validation or writing fails.
        with tempfile.NamedTemporaryFile(dir=args.output.parent, delete=False) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(document)
        os.replace(temporary_path, args.output)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
