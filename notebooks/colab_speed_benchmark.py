# ============================================================================
#  rustai vs 다른 수집·추출 라이브러리 — 속도 벤치마크 (Colab)
#
#  이 셀 하나가 설치·코퍼스 확보·세 가지 측정을 모두 수행합니다.
#
#  A. 직렬 추출   HTML → 본문 텍스트, 한 번에 한 문서
#  B. 병렬 추출   전 코어 사용. 파이썬 라이브러리는 GIL 때문에 대개 못 늘어납니다
#  C. 네트워크    실제 URL 수집. 소음이 크고 대역폭에 좌우됩니다
#
#  ── GPU는 이 벤치마크에서 놀고 있습니다 ──────────────────────────────
#  HTML 파싱과 HTTP 수집은 CPU와 네트워크에 묶인 작업입니다. T4 런타임에서
#  돌아가지만 GPU를 쓰지 않으므로, 숫자는 CPU 런타임과 같습니다. 결과에
#  영향을 주는 것은 Colab이 배정한 vCPU 수와 회선 상태입니다.
#
#  ── 공정하게 읽는 법 ────────────────────────────────────────────────
#  같은 일을 하지 않는 것끼리 속도만 비교하면 무의미합니다. 표에 출력 크기를
#  함께 싣는 이유입니다. selectolax와 bs4 는 *파서* 로, 본문을 찾지 않고
#  페이지의 모든 텍스트를 내놓습니다 -- 빠른 대신 광고와 내비게이션이 그대로
#  섞여 나옵니다. 나머지는 *추출기* 로, 본문이 어디인지 판단합니다.
#  그리고 rustai 는 이 중 어느 것도 하지 않는 일 -- 검색 라우팅, 순위,
#  토큰 예산 압축 -- 을 함께 하므로, 추출 속도만으로 전부를 판단할 수는
#  없습니다. 이 표가 답하는 질문은 좁습니다: "HTML 더미를 텍스트로 바꾸는 데
#  얼마나 걸리는가".
# ============================================================================

import concurrent.futures as cf
import contextlib
import gc
import io
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.request

REPEAT = 6          # 코퍼스를 몇 배로 늘려 잴 것인가
NET_ENABLED = True  # C 단계(네트워크)를 돌릴지

# ------------------------------------------------------------------ 설치
def sh(*args):
    r = subprocess.run([sys.executable, "-m", "pip", "install", "-q", *args],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-1500:])
    return r.returncode == 0

print("■ 설치 중… (1~2분)")
sh("-U", "rustai")
sh("trafilatura", "readability-lxml", "justext", "selectolax", "resiliparse",
   "beautifulsoup4", "lxml", "httpx", "aiohttp")

import rustai
print(f"  rustai {rustai.__version__} · python {sys.version.split()[0]} · "
      f"{os.cpu_count()} vCPU")

# 없으면 그 줄만 빠집니다. 하나가 깨져도 벤치마크 전체가 죽지 않도록.
def load(name, build):
    try:
        return build()
    except Exception as error:
        print(f"  건너뜀 {name}: {type(error).__name__}: {str(error)[:60]}")
        return None

def _trafilatura():
    import trafilatura
    return lambda h, u: trafilatura.extract(h, url=u, include_tables=True) or ""

def _readability():
    import bs4
    from readability import Document
    return lambda h, u: bs4.BeautifulSoup(Document(h).summary(), "lxml").get_text(" ")

def _justext():
    import justext
    stop = justext.get_stoplist("English")
    return lambda h, u: "\n".join(p.text for p in justext.justext(h.encode(), stop)
                                  if not p.is_boilerplate)

def _resiliparse():
    from resiliparse.extract.html2text import extract_plain_text
    from resiliparse.parse.html import HTMLTree
    return lambda h, u: extract_plain_text(HTMLTree.parse(h), main_content=True)

def _bs4():
    import bs4
    return lambda h, u: bs4.BeautifulSoup(h, "lxml").get_text(" ", strip=True)

def _selectolax():
    from selectolax.parser import HTMLParser
    def run(h, u):
        body = HTMLParser(h).body
        return body.text(separator=" ") if body else ""
    return run

EXTRACTORS = {k: v for k, v in {
    "rustai":           lambda h, u: rustai.extract(h, url=u).text,
    "resiliparse":      load("resiliparse", _resiliparse),
    "trafilatura":      load("trafilatura", _trafilatura),
    "readability-lxml": load("readability-lxml", _readability),
    "justext":          load("justext", _justext),
}.items() if v}
PARSERS = {k: v for k, v in {
    "selectolax": load("selectolax", _selectolax),
    "bs4+lxml":   load("bs4+lxml", _bs4),
}.items() if v}

# ---------------------------------------------------------------- 코퍼스
# 합성 문서가 아니라 실제 페이지입니다. 진짜 페이지는 크고 지저분하며 --
# 위키백과 한 장이 1MB를 넘습니다 -- 그 비율이 추출기가 시험받는 지점입니다.
PAGES = {
    "wiki_bm25": "https://en.wikipedia.org/wiki/Okapi_BM25",
    "wiki_rust": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "wiki_http": "https://en.wikipedia.org/wiki/HTTP",
    "wiki_ko_seoul": "https://ko.wikipedia.org/wiki/%EC%84%9C%EC%9A%B8%ED%8A%B9%EB%B3%84%EC%8B%9C",
    "wiki_ko_ml": "https://ko.wikipedia.org/wiki/%EA%B8%B0%EA%B3%84_%ED%95%99%EC%8A%B5",
    "mdn_promise": "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise",
    "mdn_flex": "https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_flexible_box_layout/Basic_concepts_of_flexbox",
    "rust_book": "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
    "python_pep8": "https://peps.python.org/pep-0008/",
    "django_docs": "https://docs.djangoproject.com/en/5.0/intro/tutorial01/",
    "fastapi": "https://fastapi.tiangolo.com/tutorial/first-steps/",
    "realpython": "https://realpython.com/python-f-strings/",
    "daleseo": "https://www.daleseo.com/python-typing/",
    "arxiv_abs": "https://arxiv.org/abs/1706.03762",
    "bbc_tech": "https://www.bbc.com/news/technology",
}
AGENT = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
         "Chrome/124.0 Safari/537.36")

def grab(item):
    name, url = item
    try:
        req = urllib.request.Request(url, headers={"User-Agent": AGENT})
        with urllib.request.urlopen(req, timeout=30) as r:
            return name, url, r.read().decode("utf-8", "replace")
    except Exception:
        return name, url, ""

print("\n■ 코퍼스 내려받는 중…")
with cf.ThreadPoolExecutor(8) as pool:
    fetched = [(u, h) for _, u, h in pool.map(grab, PAGES.items()) if h]
corpus = fetched * REPEAT
htmls = [h for _, h in corpus]
us = [u for u, _ in corpus]
MB = sum(len(h.encode()) for h in htmls) / (1 << 20)
print(f"  실제 페이지 {len(fetched)}장 × {REPEAT} = {len(corpus)}문서, {MB:.1f} MB "
      f"(평균 {MB * 1024 / len(corpus):.0f} KB/문서)")

# ------------------------------------------------------- A. 직렬 추출
def measure(fn):
    gc.collect()
    produced = 0
    start = time.perf_counter()
    for html, url in zip(htmls, us):
        try:
            # justext 등이 stderr 로 경고를 쏟습니다. 측정에는 무관합니다.
            with contextlib.redirect_stderr(io.StringIO()):
                produced += len(fn(html, url))
        except Exception:
            pass
    return time.perf_counter() - start, produced

print("\n" + "=" * 74)
print("A. 직렬 추출 — 한 번에 한 문서")
print("=" * 74)
print(f"{'library':20}{'종류':8}{'docs/s':>9}{'MB/s':>8}{'출력':>9}{'배속':>7}")
serial = {}
for group, table in (("추출기", EXTRACTORS), ("파서", PARSERS)):
    for name, fn in table.items():
        seconds, produced = measure(fn)
        serial[name] = len(corpus) / seconds
        print(f"{name:20}{group:8}{len(corpus)/seconds:>9.0f}{MB/seconds:>8.1f}"
              f"{produced/(1<<20):>8.1f}M{serial[name]/serial['rustai']:>6.1f}x")

# ------------------------------------------------------- B. 병렬 추출
# rustai 는 rayon 으로 코어에 흩고 GIL 을 놓습니다. 파이썬 라이브러리가
# 스레드에서 빨라지려면 그 라이브러리도 GIL 을 놓아야 하는데, C 확장으로
# 쓰인 것만 그렇습니다 -- 그래서 아래 결과가 갈립니다.
print("\n" + "=" * 74)
print(f"B. 병렬 추출 — {os.cpu_count()} vCPU 전부")
print("=" * 74)

def timed(label, fn):
    gc.collect()
    start = time.perf_counter()
    fn()
    seconds = time.perf_counter() - start
    print(f"  {label:44}{len(corpus)/seconds:>8.0f} docs/s{MB/seconds:>8.1f} MB/s")
    return len(corpus) / seconds

par = {}
par["rustai"] = timed("rustai.extract_many (rayon, GIL 해제)",
                      lambda: rustai.extract_many(htmls, us))
workers = os.cpu_count() or 4
for name in ("resiliparse", "trafilatura", "readability-lxml", "justext"):
    fn = EXTRACTORS.get(name)
    if not fn:
        continue
    def safe(pair, fn=fn):
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                return len(fn(*pair))
        except Exception:
            return 0
    par[name] = timed(f"{name} ThreadPool({workers})",
                      lambda: list(cf.ThreadPoolExecutor(workers)
                                   .map(safe, zip(htmls, us))))

# ------------------------------------------------------- C. 네트워크
if NET_ENABLED:
    print("\n" + "=" * 74)
    print("C. 네트워크 수집 — 서로 다른 호스트 15곳")
    print("=" * 74)
    print("  주의: rustai 는 호스트마다 robots.txt 를 먼저 확인하고 호스트별")
    print("  간격을 지킵니다. 아래 다른 라이브러리는 둘 다 하지 않습니다 --")
    print("  같은 속도가 같은 예의를 뜻하지 않습니다.")
    targets = [u for u, _ in fetched]   # fetched 는 (url, html) 입니다

    def net(label, fn):
        gc.collect()
        start = time.perf_counter()
        got = fn()
        seconds = time.perf_counter() - start
        note = "   ← 전부 실패, 회선이나 인자를 확인하세요" if got == 0 else ""
        print(f"  {label:44}{seconds:>7.1f}s  {got}/{len(targets)}건{note}")

    client = rustai.Client(timeout=30.0, concurrency=16)
    net("rustai Client.fetch (동시 16, robots 준수)",
        lambda: len(client.fetch(targets)))

    try:
        import requests
        def seq():
            n = 0
            with requests.Session() as s:
                for u in targets:
                    try:
                        n += s.get(u, headers={"User-Agent": AGENT}, timeout=30).ok
                    except Exception:
                        pass
            return n
        net("requests 순차 (robots 확인 안 함)", seq)
    except ImportError:
        pass

    # 노트북에는 이미 이벤트 루프가 돌고 있어 asyncio.run 이 거부됩니다.
    # 코루틴을 제 루프를 가진 별도 스레드에서 돌리면 어느 쪽에서든 동작합니다.
    import asyncio
    import threading

    def in_own_loop(make_coro):
        box = {}
        def worker():
            try:
                box["value"] = asyncio.run(make_coro())
            except Exception as error:
                box["error"] = error
        thread = threading.Thread(target=worker)
        thread.start()
        thread.join()
        if "error" in box:
            raise box["error"]
        return box["value"]

    try:
        import httpx
        async def with_httpx():
            async with httpx.AsyncClient(timeout=30, follow_redirects=True) as c:
                rs = await asyncio.gather(
                    *(c.get(u, headers={"User-Agent": AGENT}) for u in targets),
                    return_exceptions=True)
            return sum(1 for r in rs if not isinstance(r, Exception))
        net("httpx 비동기 (robots 확인 안 함)", lambda: in_own_loop(with_httpx))
    except Exception as error:
        print(f"  건너뜀 httpx: {type(error).__name__}: {str(error)[:50]}")

    try:
        import aiohttp
        async def with_aiohttp():
            async with aiohttp.ClientSession(
                    headers={"User-Agent": AGENT},
                    timeout=aiohttp.ClientTimeout(total=30)) as session:
                async def one(u):
                    async with session.get(u) as r:
                        await r.read()
                        return r.status < 400
                rs = await asyncio.gather(*(one(u) for u in targets),
                                          return_exceptions=True)
            return sum(1 for r in rs if r is True)
        net("aiohttp 비동기 (robots 확인 안 함)", lambda: in_own_loop(with_aiohttp))
    except Exception as error:
        print(f"  건너뜀 aiohttp: {type(error).__name__}: {str(error)[:50]}")

print("\n" + "=" * 74)
print("읽는 법")
print("=" * 74)
print("""
· 파서(selectolax, bs4)는 본문을 찾지 않습니다. 출력 크기를 보면 광고와
  내비게이션까지 통째로 들어 있는 것이 보입니다. 빠른 것이 당연합니다.
· 추출기끼리가 진짜 비교입니다. resiliparse 는 C++ 로 쓰인 추출 전용
  라이브러리이고, 이 축에서는 rustai 보다 빠릅니다.
· rustai 가 함께 하는 일 -- 13종 검색 라우팅, RRF 융합, BM25 순위,
  토큰 예산 압축 -- 은 위 어느 라이브러리도 하지 않습니다. 그 값을 이
  표는 재지 않습니다.
· 네트워크 수치는 회선과 상대 서버에 좌우됩니다. 여러 번 돌려보세요.
""")
