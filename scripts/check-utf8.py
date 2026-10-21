# LanBridge UTF-8 self-check (.ps1 requires BOM)
# Usage: python scripts/check-utf8.py [root...]
import pathlib
import sys

roots = sys.argv[1:] or [str(pathlib.Path(__file__).resolve().parent.parent)]
skip_parts = {
    ".git",
    "toolchains",
    "ide",
    ".tmp",
    "target",
    "build",
    "oh_modules",
    "libs",
    ".hvigor",
    ".cxx",
    "node_modules",
}
text_suffix = {
    ".md",
    ".mdc",
    ".json",
    ".json5",
    ".toml",
    ".ps1",
    ".sh",
    ".rs",
    ".py",
    ".txt",
    ".yml",
    ".yaml",
    ".ets",
    ".ts",
    ".tsx",
    ".vue",
    ".mdx",
    ".cpp",
    ".h",
    ".cmake",
}

bad = []
checked = 0
for root in roots:
    for p in pathlib.Path(root).rglob("*"):
        if not p.is_file():
            continue
        if any(part in skip_parts for part in p.parts):
            continue
        if p.suffix.lower() not in text_suffix and not p.name.startswith("."):
            continue
        try:
            data = p.read_bytes()
            text = data.decode("utf-8")
            checked += 1
            if "\ufffd" in text:
                bad.append((str(p), "contains U+FFFD replacement character"))
            if p.suffix.lower() == ".ps1" and not data.startswith(b"\xef\xbb\xbf"):
                bad.append((str(p), "missing UTF-8 BOM (PowerShell 5.1 requires it)"))
        except Exception as e:
            bad.append((str(p), repr(e)))

print(f"checked={checked}")
if bad:
    print("FAIL:")
    for path, err in bad:
        print(f"  {path}: {err}")
    raise SystemExit(1)
print("OK: all text files utf-8 clean; .ps1 files have BOM")
