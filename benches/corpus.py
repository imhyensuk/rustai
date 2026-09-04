"""Generate a reproducible, realistic HTML corpus for benchmarking.

Synthetic rather than scraped so the benchmark is deterministic, redistributable
and free of anyone's copyright. The shape is what matters and it is modelled on
real article pages: a heavy chrome-to-content ratio, deeply nested wrapper divs,
inline SVG icons, a script blob, and a body that is a small fraction of the byte
count. That ratio -- not raw size -- is what an extractor is actually tested on.
"""

import random

LOREM = (
    "The parser walks the arena exactly once and caches three metrics per node, "
    "which is what makes a per-node text-to-markup ratio affordable at all. "
    "Resident memory stayed at 28 MB across 1,200 documents on 8 cores. "
    "Latency is dominated by the network, not by parsing, once the DOM is flat. "
)
NAV = "".join(f'<li><a href="/section/{i}">Section {i}</a></li>' for i in range(24))
ICON = '<svg viewBox="0 0 24 24"><path d="M12 2L2 7l10 5 10-5-10-5z"/></svg>'


def _ad(i: int) -> str:
    return (
        f'<div class="ad-slot ad-slot--{i}" data-ad-unit="/1234/banner">'
        f'<div class="ad-wrapper"><div class="ad-inner"><ins class="adsbygoogle" '
        f'data-ad-client="ca-pub-{i}" data-ad-slot="{i}"></ins></div></div></div>'
    )


def document(seed: int, paragraphs: int = 45) -> str:
    rng = random.Random(seed)
    body = []
    for p in range(paragraphs):
        if p and p % 6 == 0:
            body.append(f"<h2>Section heading {p // 6}</h2>")
        sentences = " ".join(rng.sample(LOREM.split(". "), 3))
        body.append(f"<p>{sentences}. Figure {p} shows {rng.randint(10, 999)} results.</p>")
        if p % 9 == 4:
            body.append(_ad(p))
    body.append("<ul>" + "".join(f"<li>Bullet {i}</li>" for i in range(6)) + "</ul>")
    body.append('<pre><code class="language-rust">let doc = Doc::parse(html)?;</code></pre>')

    return f"""<!doctype html><html lang="en"><head>
<meta charset="utf-8"><title>Benchmark document {seed} — Example Wire</title>
<meta property="og:site_name" content="Example Wire">
<meta name="description" content="A synthetic article for benchmarking extraction.">
<meta property="article:published_time" content="2026-09-0{seed % 9 + 1}T00:00:00Z">
<script type="application/ld+json">{{"@type":"Article","headline":"Benchmark document {seed}","author":{{"name":"A. Author"}}}}</script>
<style>{'.c{margin:0;padding:0}' * 900}</style>
</head><body>
<header class="site-header"><div class="masthead"><div class="brand">{ICON}Example Wire</div>
<nav class="main-nav" role="navigation"><ul>{NAV}</ul></nav></div></header>
<div class="cookie-consent-banner"><p>We use cookies. Accept cookies to continue.</p><button>Accept</button></div>
{_ad(0)}
<div class="page"><div class="page__inner"><div class="grid"><div class="grid__main">
<article class="article post-content" itemscope>
<h1>Benchmark document {seed}</h1>
<div class="byline">By A. Author · <time>2026-09-01</time> · <span class="share">Share this</span></div>
{''.join(body)}
</article></div>
<aside class="grid__rail sidebar"><div class="widget widget--related"><h3>Related</h3><ul>
{''.join(f'<li><a href="/r/{i}">Related story {i}</a></li>' for i in range(15))}
</ul></div><div class="widget widget--newsletter"><h3>Newsletter</h3>
<form><input type="email"><button>Subscribe to our newsletter</button></form></div>{_ad(99)}</aside>
</div></div></div>
<footer class="site-footer"><nav><ul>{NAV}</ul></nav>
<p>© 2026 Example Wire. All rights reserved. Privacy policy. Terms of service.</p></footer>
<script>window.__DATA__={{"tracking":[{','.join(str(i) for i in range(2200))}]}};</script>
</body></html>"""


def corpus(n: int = 200) -> list[str]:
    """A deterministic corpus of `n` documents."""
    return [document(i) for i in range(n)]


def write(directory: str, n: int = 200) -> None:
    """Dump the corpus as files, so the Rust benchmark can stream it from disk."""
    import pathlib

    out = pathlib.Path(directory)
    out.mkdir(parents=True, exist_ok=True)
    for i, doc in enumerate(corpus(n)):
        (out / f"{i:04}.html").write_text(doc, encoding="utf-8")
    print(f"wrote {n} documents to {out}")


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1:
        write(sys.argv[1])
    else:
        docs = corpus(200)
        total = sum(len(d) for d in docs)
        print(f"{len(docs)} documents, {total / 1e6:.2f} MB, mean {total / len(docs) / 1024:.0f} KB")
