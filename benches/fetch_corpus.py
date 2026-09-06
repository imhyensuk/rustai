#!/usr/bin/env python3
"""Fetch the corpus that `quality.py` scores.

The pages are downloaded rather than vendored: they are other people's
copyrighted HTML, and a checked-in copy would rot against the live sites
the extractor actually has to survive. What *is* checked in is the list --
so the corpus is reproducible even though its contents are not frozen.

The list is chosen for the shapes that break extractors, not for variety:
MediaWiki's deep template nesting, MDN's lit-html comments and `<?>` holes,
documentation sites whose prose sits inside layout wrappers, a news front
page that is a link inventory rather than an article, and Korean pages so
the CJK path is exercised. Some will 403 or move; the script reports what
it got and scoring simply skips what is missing.

    python3 benches/fetch_corpus.py /tmp/rustai-pages
    python3 benches/quality.py      /tmp/rustai-pages
"""

import concurrent.futures
import json
import pathlib
import sys
import urllib.error
import urllib.request

PAGES = {
    # MediaWiki: heavy templates, navboxes, citation machinery.
    "wiki_bm25": "https://en.wikipedia.org/wiki/Okapi_BM25",
    "wiki_rust": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "wiki_transformer": "https://en.wikipedia.org/wiki/Transformer_(deep_learning_architecture)",
    "wiki_tfidf": "https://en.wikipedia.org/wiki/Tf%E2%80%93idf",
    "wiki_http": "https://en.wikipedia.org/wiki/HTTP",
    # Korean: the CJK tokenisation and furniture vocabulary.
    "wiki_ko_seoul": "https://ko.wikipedia.org/wiki/%EC%84%9C%EC%9A%B8%ED%8A%B9%EB%B3%84%EC%8B%9C",
    "wiki_ko_rust": "https://ko.wikipedia.org/wiki/%EB%9F%AC%EC%8A%A4%ED%8A%B8_(%ED%94%84%EB%A1%9C%EA%B7%B8%EB%9E%98%EB%B0%8D_%EC%96%B8%EC%96%B4)",
    "wiki_ko_ml": "https://ko.wikipedia.org/wiki/%EA%B8%B0%EA%B3%84_%ED%95%99%EC%8A%B5",
    "daleseo": "https://www.daleseo.com/python-typing/",
    # MDN: lit-html comment markers, and the `<?>` that cost the page its
    # examples until the bogus-comment repair landed.
    "mdn_fetch": "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch",
    "mdn_flex": "https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_flexible_box_layout/Basic_concepts_of_flexbox",
    "mdn_promise": "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise",
    # Documentation: prose inside layout wrappers, syntax-highlighted code.
    "rust_book_ch4": "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
    "python_pep8": "https://peps.python.org/pep-0008/",
    "django_docs": "https://docs.djangoproject.com/en/5.0/intro/tutorial01/",
    "fastapi": "https://fastapi.tiangolo.com/tutorial/first-steps/",
    "realpython": "https://realpython.com/python-f-strings/",
    # An abstract page, and a front page that is a link inventory.
    "arxiv_abs": "https://arxiv.org/abs/1706.03762",
    "nature_news": "https://www.nature.com/articles/d41586-024-00189-3",
    "bbc_tech": "https://www.bbc.com/news/technology",
    "hn_item": "https://news.ycombinator.com/item?id=40000000",
}

AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/124.0 Safari/537.36"
)


def fetch(item: tuple[str, str], directory: pathlib.Path) -> tuple[str, int, str]:
    name, url = item
    request = urllib.request.Request(
        url, headers={"User-Agent": AGENT, "Accept-Language": "en,ko;q=0.8"}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            html = response.read().decode("utf-8", "replace")
    except (urllib.error.URLError, OSError, TimeoutError) as error:
        return name, 0, str(error)[:60]
    (directory / f"{name}.html").write_text(html, encoding="utf-8")
    return name, len(html), ""


def main() -> None:
    directory = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/rustai-pages")
    directory.mkdir(parents=True, exist_ok=True)

    got = 0
    # Modest concurrency: this is someone else's bandwidth, and the corpus is
    # two dozen pages rather than a crawl.
    with concurrent.futures.ThreadPoolExecutor(6) as pool:
        for name, size, error in pool.map(lambda i: fetch(i, directory), PAGES.items()):
            print(f"{name:20} {size:>9,}  {error}")
            got += size > 0

    (directory / "urls.json").write_text(json.dumps(PAGES, indent=1))
    print(f"\n{got}/{len(PAGES)} pages into {directory}")
    if got < len(PAGES):
        print("Pages that failed are skipped by scoring; a few 403s are normal.")


if __name__ == "__main__":
    main()
