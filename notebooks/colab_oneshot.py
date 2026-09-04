# ══════════════════════════════════════════════════════════════════════════
#  rustai — 코랩 통합 셀 (빌드 + 설치 + 리포트)
#
#  이 셀 하나만 실행하면 됩니다. 최초 1회는 소스 빌드 때문에 10~20분 걸리고,
#  드라이브 캐시를 켜 두면 다음 세션부터는 수십 초면 끝납니다.
#
#  끝나면 출력 전체를 복사해서 알려주세요 (/content/rustai-report.txt 로도 저장).
# ══════════════════════════════════════════════════════════════════════════

GITHUB_REPO     = ""                  # 예: "hyeonseok-im/rustai". 비우면 업로드한 sdist 사용
USE_DRIVE_CACHE = True                # 빌드한 휠을 구글 드라이브에 저장해 재사용
FAST_BUILD      = True                # LTO를 꺼서 빌드 시간 단축 (동작은 동일)
FORCE_REBUILD   = False               # 캐시를 무시하고 다시 빌드
CONTACT_EMAIL   = "you@example.com"   # OpenAlex/Crossref 폴라이트 풀용

import glob, os, platform, shutil, subprocess, sys, textwrap, time, traceback

IN_COLAB = "google.colab" in sys.modules or os.path.isdir("/content")
WORK     = "/content" if os.path.isdir("/content") else os.getcwd()
LOGDIR   = os.path.join(WORK, "rustai-logs"); os.makedirs(LOGDIR, exist_ok=True)

def sh(cmd, log):
    """셸 명령 실행. 실패하면 로그 꼬리를 보여주고 멈춥니다."""
    path = os.path.join(LOGDIR, log)
    with open(path, "wb") as f:
        r = subprocess.run(cmd, shell=True, stdout=f, stderr=subprocess.STDOUT,
                           executable="/bin/bash")
    if r.returncode:
        print(f"\n╳ 실패 (exit {r.returncode}): {cmd}")
        print(f"─ {path} 마지막 60줄 " + "─" * 30)
        print("".join(open(path, errors="replace").readlines()[-60:]))
        raise SystemExit(f"중단됨. 전체 로그: {path}")
    return path

# ── 1. 설치 ───────────────────────────────────────────────────────────────
def ensure_rustai():
    if not FORCE_REBUILD:
        try:
            import rustai                                    # noqa: F401
            print("이미 설치되어 있습니다 — 빌드를 건너뜁니다.")
            return
        except ImportError:
            pass

    cache = None
    if USE_DRIVE_CACHE and IN_COLAB:
        try:
            from google.colab import drive
            drive.mount("/content/drive")
            cache = "/content/drive/MyDrive/rustai-wheels"
            os.makedirs(cache, exist_ok=True)
        except Exception as e:
            print("드라이브 사용 불가, 캐시 없이 진행합니다:", e)

    hits = sorted(glob.glob(f"{cache}/rustai-*.whl")) if cache else []
    if hits and not FORCE_REBUILD:
        wheel = hits[-1]
        print("캐시된 휠 사용:", os.path.basename(wheel))
    else:
        # 소스 위치
        if GITHUB_REPO:
            if not os.path.isdir(f"{WORK}/rustai-src"):
                print("[1/5] 소스 clone")
                sh(f"git clone --depth 1 https://github.com/{GITHUB_REPO}.git "
                   f"{WORK}/rustai-src", "clone.log")
            source = f"{WORK}/rustai-src"
        else:
            found = sorted(glob.glob(f"{WORK}/rustai-*.tar.gz")
                           + glob.glob("rustai-*.tar.gz"))
            if not found:
                raise SystemExit(
                    "소스를 찾지 못했습니다.\n"
                    "  · 왼쪽 파일 탭에 rustai-0.1.0.tar.gz 를 업로드하거나\n"
                    "  · 위의 GITHUB_REPO 를 채워주세요.")
            source = os.path.abspath(found[-1])
        print("소스:", source)

        # BoringSSL(btls-sys)의 빌드 스크립트는 cmake 와 bindgen 을 씁니다.
        # bindgen 은 libclang 을 필요로 하는데 코랩 기본 이미지에 없어서,
        # 이걸 빼면 "Unable to find libclang" 으로 실패합니다.
        print("[2/5] 빌드 도구 설치 (cmake, clang, libclang-dev)")
        sh("apt-get -qq update && apt-get -qq install -y "
           "cmake clang libclang-dev pkg-config", "apt.log")

        print("[3/5] 러스트 1.98 툴체인 설치")
        sh("curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal "
           "--default-toolchain 1.98.0", "rustup.log")
        os.environ["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + os.environ["PATH"]

        sh(f'"{sys.executable}" -m pip -q install "maturin>=1.7,<2.0"', "maturin.log")
        # --no-build-isolation 은 maturin 을 PATH 의 실행 파일로 찾습니다. 임포트만
        # 되는 걸로는 부족하고, 스크립트 설치 위치는 인터프리터 디렉터리와 다를 수
        # 있습니다 (코랩: 파이썬 /usr/bin, 스크립트 /usr/local/bin).
        import site, sysconfig
        for d in (sysconfig.get_path("scripts"),
                  sysconfig.get_path("scripts", f"{os.name}_user"),
                  os.path.join(site.getuserbase(), "bin"), "/usr/local/bin"):
            if d and os.path.isdir(d) and d not in os.environ["PATH"].split(os.pathsep):
                os.environ["PATH"] = d + os.pathsep + os.environ["PATH"]
        if not shutil.which("maturin"):
            raise SystemExit("maturin 실행 파일을 PATH 에서 찾지 못했습니다:\n"
                             + os.environ["PATH"])

        if FAST_BUILD:
            os.environ["CARGO_PROFILE_RELEASE_LTO"] = "false"
            os.environ["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"] = "16"

        print(f"[4/5] 휠 빌드 — 최초 1회 10~20분. 진행 상황: "
              f"!tail -5 {LOGDIR}/build.log")
        wh = os.path.join(WORK, "wheelhouse"); os.makedirs(wh, exist_ok=True)
        t0 = time.perf_counter()
        sh(f'"{sys.executable}" -m pip wheel --no-deps --no-build-isolation '
           f'-w "{wh}" -v "{source}"', "build.log")
        built = sorted(glob.glob(f"{wh}/rustai-*.whl"))
        if not built:
            raise SystemExit(f"휠이 생성되지 않았습니다. {LOGDIR}/build.log 확인")
        wheel = built[-1]
        print(f"      빌드 완료: {(time.perf_counter()-t0)/60:.1f}분  "
              f"{os.path.basename(wheel)}")
        if cache:
            shutil.copy(wheel, cache)
            print("      캐시 저장:", cache)

    print("[5/5] 설치:", os.path.basename(wheel))
    sh(f'"{sys.executable}" -m pip -q install --force-reinstall "{wheel}"', "install.log")

ensure_rustai()
import rustai

# ── 2. 리포트 ─────────────────────────────────────────────────────────────
OUT = []
def p(*a):
    line = " ".join(str(x) for x in a)
    OUT.append(line); print(line)

def rss_mb():
    import resource
    r = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return r / 1024 if sys.platform.startswith("linux") else r / 1024 / 1024

p("\n" + "=" * 64)
p("[환경]")
p(f"  rustai        {rustai.__version__}")
p(f"  python        {sys.version.split()[0]}   {platform.machine()}  {platform.system()}")
p(f"  cpu           {os.cpu_count()} core")
try:
    mt = [l for l in open("/proc/meminfo") if l.startswith("MemTotal")][0].split()[1]
    p(f"  ram           {int(mt)/1024/1024:.1f} GiB")
except Exception:
    pass
try:
    p("  rustc         " + subprocess.run(["rustc", "--version"],
                                          capture_output=True, text=True).stdout.strip())
except Exception:
    p("  rustc         (없음 — 캐시된 휠로 설치됨)")
w = sorted(glob.glob(f"{WORK}/wheelhouse/rustai-*.whl")
           + glob.glob("/content/drive/MyDrive/rustai-wheels/rustai-*.whl"))
p(f"  wheel         {os.path.basename(w[-1]) if w else '(경로 못 찾음)'}")

HTML = """<html lang="ko"><head><title>테스트 — 예시 블로그</title>
<meta property="og:site_name" content="예시 블로그"><meta name="author" content="홍길동"></head><body>
<header class="gnb"><nav><a href="/a">메뉴1</a><a href="/b">메뉴2</a><a href="/c">메뉴3</a></nav></header>
<div class="ad-slot" data-ad-unit="top"><ins class="adsbygoogle">광고 문구</ins></div>
<main><article class="post-content"><h1>제로코스트 크롤링</h1>
<p>파이썬 크롤러는 요청하지 않은 DOM 비용을 지불하며, 이는 메모리 점유율과 지연 시간에 그대로 드러납니다.</p>
<p>아보가드로 수는 6.02 × 10<sup>23</sup> mol<sup>-1</sup> 이고 오차는 ±0.5 °C 입니다.</p>
<pre><code class="language-rust">let doc = Doc::parse(html)?;</code></pre>
<table><tr><th>엔진</th><th>피크 RSS</th></tr><tr><td>arena</td><td>4.8 MiB</td></tr>
<tr><td>gc</td><td>412 MB</td></tr></table>
<form><input placeholder="구독하기"><button>구독하기</button></form>
<aside class="sponsored-content">스폰서드 콘텐츠입니다</aside></article></main>
<footer>무단 전재 및 재배포 금지</footer></body></html>"""

p("")
p("[1. 오프라인 정제]")
try:
    a = rustai.extract(HTML, url="https://example.com/post")
    checks = [("코드 펜스 language=rust", "```rust" in a.markdown),
              ("GFM 표 변환",  "| 엔진 |" in a.markdown or "|엔진|" in a.markdown),
              ("지수 10^23 보존",   "10^23" in a.markdown),
              ("단위 mol^-1 보존",  "mol^-1" in a.markdown),
              ("수치 4.8 MiB 보존", "4.8 MiB" in a.markdown)]
    for junk in ("메뉴1", "광고 문구", "스폰서드", "구독하기", "무단 전재"):
        checks.append((f"제외: {junk}", junk not in a.text))
    for name, ok in checks:
        p(f"  {'OK  ' if ok else '실패'}  {name}")
    p(f"  kind={a.kind}  units={len(a.units)}  tokens={a.tokens}  "
      f"compression={a.stats.compression:.1%}")
except Exception:
    p("  예외:"); p(traceback.format_exc())

p("")
p("[2. 처리량]  ※ 작은 픽스처 기준. 같은 셀끼리만 비교하세요")
try:
    docs = [HTML] * 400
    t = time.perf_counter(); [rustai.extract(d) for d in docs]
    s1 = len(docs) / (time.perf_counter() - t)
    t = time.perf_counter(); rustai.extract_many(docs)
    s2 = len(docs) / (time.perf_counter() - t)
    p(f"  extract       {s1:8.0f} docs/s")
    p(f"  extract_many  {s2:8.0f} docs/s  ({s2/s1:.1f}x)")
    p(f"  peak RSS      {rss_mb():8.1f} MB  (파이썬 인터프리터 포함)")
except Exception:
    p("  예외:"); p(traceback.format_exc())

p("")
p("[3. 실제 수집 — 코랩 IP가 차단되는지]")
try:
    client = rustai.Client(timeout=30.0, contact_email=CONTACT_EMAIL)
    for u in ["https://en.wikipedia.org/wiki/BM25",
              "https://arxiv.org/abs/1706.03762",
              "https://doc.rust-lang.org/book/ch01-01-installation.html",
              "https://news.ycombinator.com/"]:
        try:
            t = time.perf_counter()
            art = client.read([u], raise_on_error=True)[0]
            ms = (time.perf_counter() - t) * 1000
            extra = f"{len(art.links)}링크" if art.kind == "index" else f"{art.tokens}토큰"
            p(f"  OK    {ms:6.0f}ms  {art.kind:7} {extra:>9}  {u}")
        except Exception as e:
            p(f"  실패            {type(e).__name__}: {str(e)[:70]}  {u}")
except Exception:
    p("  예외:"); p(traceback.format_exc())

p("")
p("[4. 검색 프로바이더 — 구글 데이터센터 IP에서 되는지]")
for prov in ["duckduckgo", "wikipedia:ko", "wikipedia:en", "arxiv", "openalex", "crossref"]:
    try:
        c = rustai.Client(providers=[prov], limit=5, timeout=30.0,
                          contact_email=CONTACT_EMAIL)
        t = time.perf_counter()
        r = c.search("BM25 ranking function", strict=True)
        p(f"  {'OK  ' if r else '0건 '}  {(time.perf_counter()-t)*1000:6.0f}ms  "
          f"{len(r)}건  {prov:14} {(r[0].title[:38] if r else '')!r}")
    except Exception as e:
        p(f"  실패            {prov:14} {type(e).__name__}: {str(e)[:60]}")

p("")
p("[5. 전체 파이프라인]")
try:
    t = time.perf_counter()
    res = rustai.research("BM25 랭킹 함수는 문서 길이를 어떻게 다루는가",
                          max_sources=4, max_tokens=1200, contact_email=CONTACT_EMAIL)
    dt = time.perf_counter() - t
    raw = sum(art.stats.html_bytes for art in res.articles)
    p(f"  {dt:.1f}s   검색 {len(res.results)}건 → 수집 {len(res.articles)}건 → "
      f"컨텍스트 {res.context.tokens}/1200 토큰")
    p(f"  원본 {raw/1024:.0f} KB → 컨텍스트 {len(res.markdown)/1024:.1f} KB")
    p(f"  실패한 소스: {res.failures if res.failures else '없음'}")
    p("  ─ 컨텍스트 앞 400자 " + "─" * 26)
    for line in res.markdown[:400].splitlines():
        p("  | " + line)
except Exception:
    p("  예외:"); p(traceback.format_exc())

p("=" * 64)
report = os.path.join(WORK, "rustai-report.txt")
open(report, "w").write("\n".join(OUT))
print(f"\n위 출력 전체를 복사해서 알려주세요. 파일로도 저장됨: {report}")
