"""Read-only verification of the saved remote/mobile research evidence."""
import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path
from urllib.parse import unquote

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--luna-root", type=Path)
    parser.add_argument("--t3-root", type=Path)
    args = parser.parse_args()
    package = Path(__file__).resolve().parent
    luna = (args.luna_root or package.parents[2]).resolve()
    t3 = (args.t3_root or luna.parent / "t3code").resolve()
    manifest = json.loads((package / "evidence-manifest.json").read_text())
    repos = {"novnski/luna": luna, "pingdotgg/t3code": t3}
    errors = []
    source_data = {}
    for item in manifest["source_files"]:
        repo, rel, commit = item["repository"], item["path"], item["commit"]
        proc = subprocess.run(
            ["git", "-C", str(repos[repo]), "show", f"{commit}:{rel}"],
            capture_output=True,
            check=False,
        )
        if proc.returncode:
            errors.append(f"source unavailable: {repo}/{rel} at {commit}")
            continue
        data = proc.stdout
        source_data[(repo, rel)] = data
        if hashlib.sha256(data).hexdigest() != item["sha256"]:
            errors.append(f"source hash differs: {repo}/{rel}")
        if len(data.splitlines()) != item["lines"]:
            errors.append(f"source line count differs: {repo}/{rel}")
    for item in manifest["source_citations"]:
        data = source_data.get((item["repo"], item["path"]))
        if data is not None and not 1 <= item["start"] <= item["end"] <= len(data.splitlines()):
            errors.append(f"citation bounds invalid: {item}")
    for rel, expected in {
        **manifest["document_sha256"], **manifest.get("artifact_sha256", {})
    }.items():
        path = package / rel
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            errors.append(f"document/artifact missing or changed: {rel}")
    virtual = luna / "docs/pending-tasks/t3-remote-mobile"
    link_count = 0
    for doc in sorted(package.glob("*.md")):
        for match in re.finditer(r"\[[^\]\n]*\]\(([^)\n]+)\)", doc.read_text()):
            dest = match.group(1).split(" ", 1)[0].strip("<>")
            if dest.startswith(("https://", "http://", "mailto:", "#")):
                continue
            dest = unquote(dest.split("#", 1)[0])
            target = (virtual / dest).resolve()
            actual = package / target.relative_to(virtual) if target.is_relative_to(virtual) else target
            if not actual.is_file():
                errors.append(f"broken local link: {doc.name} -> {dest}")
            link_count += 1
    result = {
        "source_files": len(manifest["source_files"]),
        "source_citations": len(manifest["source_citations"]),
        "markdown_documents": len(manifest["document_sha256"]),
        "local_links": link_count,
        "errors": errors,
        "runtime_verified": False,
    }
    print(json.dumps(result, indent=2))
    return 1 if errors else 0

if __name__ == "__main__":
    raise SystemExit(main())
