#!/usr/bin/env python3
"""Vendor every CDN file the dashboard loads into crates/amux-server/fork-cdn/files (embedded in the server), pinned by sha256 in lock.tsv beside it.

usage: fork/cdn-vendor.py            download what is missing or new, refuse a file whose bytes differ from the lock
       fork/cdn-vendor.py --check    offline: list CDN URLs in the dashboard that are not vendored (exit 1 if any)
       fork/cdn-vendor.py --update   re-download every locked URL and accept changed bytes (an owner decision)
The server rewrites these hosts to /cdn/<host>/... (crates/amux-server/src/api/fork_cdn.rs), so the browser never contacts them.
"""
import hashlib, json, os, re, sys, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATIC = os.path.join(ROOT, "crates", "amux-dashboard", "static")
OUT = os.path.join(ROOT, "crates", "amux-server", "fork-cdn", "files")
LOCK = os.path.join(ROOT, "crates", "amux-server", "fork-cdn", "lock.tsv")
HOSTS = ("cdn.jsdelivr.net", "unpkg.com", "cdnjs.cloudflare.com", "esm.sh")
URL_RE = re.compile(r"https://(?:%s)/[^\"'`\s)]*" % "|".join(re.escape(h) for h in HOSTS))
# esm.sh builds bundles per request; mermaid-to-excalidraw in the drawing tool stays unavailable rather than reach out.
UNSUPPORTED = ("https://esm.sh/",)


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def sources():
    for dirpath, _, names in os.walk(STATIC):
        for n in names:
            if n.endswith((".js", ".mjs", ".html", ".css")):
                yield os.path.join(dirpath, n)


def seeds():
    urls, prefixes = set(), set()
    for path in sources():
        for u in URL_RE.findall(read(path)):
            u = u.rstrip("\\,;")
            if u.startswith(UNSUPPORTED):
                continue
            (prefixes if u.endswith("/") else urls).add(u)
    app = read(os.path.join(STATIC, "app.js"))
    handled = set()
    hljs = "https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.9.0/languages/"
    if hljs in prefixes:
        handled.add(hljs)
        urls |= {hljs + n for n in hljs_languages()}
    for base in [p for p in prefixes if re.search(r"/katex@[^/]+/dist/$", p)]:
        handled.add(base)
        body = function_body(app, "_katexLoad")
        urls |= {base + rel for rel in re.findall(r"base \+ '([^']+)'", body)}
    if "https://unpkg.com/" in prefixes:
        handled.add("https://unpkg.com/")
        urls |= univer_urls(app)
    left = prefixes - handled
    if left:
        sys.exit("unhandled CDN prefix (add an expansion to fork/cdn-vendor.py): " + ", ".join(sorted(left)))
    return urls


def function_body(src, name):
    i = src.find("function " + name + "(")
    if i < 0:
        sys.exit(f"{name} is gone from app.js; update fork/cdn-vendor.py")
    return src[i:src.find("\n}\n", i)]


def const_list(src, name):
    m = re.search(r"const %s = \[(.*?)\];" % name, src, re.S)
    if not m:
        sys.exit(f"{name} is gone from app.js; update fork/cdn-vendor.py")
    return re.findall(r"'([^']+)'", m.group(1))


def univer_urls(app):
    u = "https://unpkg.com/"
    v = re.search(r"const _UNIVER_V = '([^']+)'", app).group(1)
    out = {f"{u}@univerjs/{p}@{v}/lib/index.css" for p in const_list(app, "_UNIVER_CSS")}
    out |= {f"{u}@univerjs/{p}@{v}/lib/umd/index.js" for p in const_list(app, "_UNIVER_PKGS")}
    out |= {f"{u}@univerjs/{p}@{v}/lib/umd/facade.js" for p in const_list(app, "_UNIVER_FACADES")}
    out |= {f"{u}@univerjs/{p}@{v}/lib/umd/locale/en-US.js" for p in const_list(app, "_UNIVER_LOCALES")}
    out |= {u + rel for rel in re.findall(r"U \+ '([^'@][^']*)'", function_body(app, "_loadUniver"))}
    return out


def hljs_languages():
    meta = json.loads(fetch("https://api.cdnjs.com/libraries/highlight.js/11.9.0?fields=files"))
    return sorted(f.split("/", 1)[1] for f in meta["files"] if f.startswith("languages/") and f.endswith(".min.js"))


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "amux-fork-vendor"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def local_path(url):
    p = urllib.parse.urlsplit(url)
    rel = p.netloc + urllib.parse.unquote(p.path)
    if ".." in rel.split("/"):
        sys.exit(f"refusing a path outside the vendor folder: {url}")
    return os.path.join(OUT, rel)


def relatives(url, data):
    text = data.decode("utf-8", errors="replace")
    refs = []
    if url.endswith(".css"):
        refs = re.findall(r"url\(\s*['\"]?(?!data:|#|https?:|//)([^'\")?#]+)", text)
    elif url.endswith((".js", ".mjs")):
        refs = re.findall(r"(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)[\"'](\.{1,2}/[^\"']+)[\"']", text)
    return {urllib.parse.urljoin(url, r) for r in refs}


def load_lock():
    lock = {}
    if os.path.exists(LOCK):
        for line in read(LOCK).splitlines():
            if line and not line.startswith("#"):
                url, digest, _ = line.split("\t")
                lock[url] = digest
    return lock


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    lock = load_lock()
    if mode == "--check":
        missing = sorted(u for u in URL_RE.findall("\n".join(read(p) for p in sources()))
                         if not u.endswith("/") and not u.startswith(UNSUPPORTED) and u.rstrip("\\,;") not in lock)
        for u in missing:
            print("NOT VENDORED", u)
        print(f"verdict={'cdn_unvendored' if missing else 'cdn_vendored'} locked={len(lock)} missing={len(missing)}")
        sys.exit(1 if missing else 0)
    queue, seen, changed = sorted(seeds()), set(), []
    while queue:
        url = queue.pop()
        if url in seen:
            continue
        seen.add(url)
        path = local_path(url)
        if os.path.exists(path) and url in lock and mode != "--update":
            data = open(path, "rb").read()
        else:
            data = fetch(url)
        digest = hashlib.sha256(data).hexdigest()
        if url in lock and lock[url] != digest:
            if mode != "--update":
                changed.append(url)
                continue
        lock[url] = digest
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data)
        queue.extend(sorted(relatives(url, data) - seen))
    for url in [u for u in lock if u not in seen]:
        del lock[url]
        if os.path.exists(local_path(url)):
            os.remove(local_path(url))
    with open(LOCK, "w") as f:
        f.write("# url\tsha256\tbytes — written by fork/cdn-vendor.py\n")
        for url in sorted(lock):
            f.write(f"{url}\t{lock[url]}\t{os.path.getsize(local_path(url))}\n")
    if changed:
        print("CHANGED BYTES (kept the locked copy; rerun with --update only after reviewing):", *changed, sep="\n  ")
        sys.exit(1)
    print(f"verdict=cdn_vendored files={len(lock)}")


if __name__ == "__main__":
    main()
