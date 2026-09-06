# ============================================================================
#  rustai × 로컬 SLM — 근거 기반 한국어 질의응답 (Colab T4)
#
#  이 셀 하나로 끝납니다:
#    1) rustai 설치        2) 구글 드라이브에 모델 캐시
#    3) SLM 로드 (T4/fp16) 4) 검색·정제·압축  5) 대화 루프
#
#  핵심은 "데모"가 아니라 측정입니다. 질문마다 같은 모델로 두 번 답합니다 —
#  근거 없이 한 번, rustai가 모아준 근거로 한 번. 라이브러리가 실제로 도움이
#  되는지는 그 둘을 나란히 놓고 봐야 알 수 있습니다.
#
#  런타임 → 런타임 유형 변경 → T4 GPU 로 설정한 뒤 실행하세요.
# ============================================================================

# ----------------------------------------------------------------- 0. 설정
MODEL_KEY = "qwen3b"          # 아래 MODELS 중 하나

MODELS = {
    # 3B / fp16 ≈ 6.2GB. T4(16GB)에 여유롭게 들어가고 한국어가 준수합니다.
    "qwen3b":  {"repo": "Qwen/Qwen2.5-3B-Instruct",            "load_4bit": False},
    # 한국어 특화. LG AI Research, 한·영 이중언어로 학습돼 존댓말이 자연스럽습니다.
    "exaone":  {"repo": "LGAI-EXAONE/EXAONE-3.5-2.4B-Instruct", "load_4bit": False},
    # 품질은 가장 좋지만 4bit 양자화가 필요하고 생성이 느립니다.
    "qwen7b":  {"repo": "Qwen/Qwen2.5-7B-Instruct",            "load_4bit": True},
}

DRIVE_DIR      = "/content/drive/MyDrive/rustai-models"  # 모델을 둘 곳
CONTEXT_TOKENS = 2048        # SLM에 넣을 근거 예산
MAX_SOURCES    = 5           # 실제로 읽어올 페이지 수
MAX_RESULTS    = 15          # 검색이 돌려줄 히트 수 (읽기 전이라 저렴)
PER_SOURCE_CAP = 700         # 한 페이지가 근거를 독점하지 못하게
MAX_NEW_TOKENS = 512
CONTACT_EMAIL  = None        # OpenAlex/Crossref polite pool 용, 선택
PROVIDERS      = ["duckduckgo", "wikipedia:ko", "wikipedia:en",
                  "stackexchange", "hackernews", "github"]
COMPARE        = True        # 근거 없는 답과 나란히 비교

# ------------------------------------------------------------- 1. 설치
import subprocess, sys, os, time, textwrap, gc

def sh(*args, quiet=True):
    r = subprocess.run([sys.executable, "-m", *args], capture_output=True, text=True)
    if r.returncode:
        print(r.stdout[-2000:]); print(r.stderr[-2000:])
        raise SystemExit(f"설치 실패: {' '.join(args)}")
    return r

print("■ 패키지 설치 중…")
sh("pip", "install", "-q", "-U", "rustai", "transformers", "accelerate", "huggingface_hub")
if MODELS[MODEL_KEY]["load_4bit"]:
    sh("pip", "install", "-q", "-U", "bitsandbytes")

import torch, rustai
print(f"  rustai {rustai.__version__} · torch {torch.__version__}")

if not torch.cuda.is_available():
    raise SystemExit("GPU가 없습니다. 런타임 → 런타임 유형 변경 → T4 GPU 로 바꾸고 다시 실행하세요.")
gpu = torch.cuda.get_device_name(0)
vram = torch.cuda.get_device_properties(0).total_memory / 1e9
print(f"  GPU: {gpu} ({vram:.0f}GB)")
# T4는 compute capability 7.5 — bf16을 지원하지 않으므로 fp16을 씁니다.
DTYPE = torch.bfloat16 if torch.cuda.is_bf16_supported() else torch.float16
print(f"  dtype: {str(DTYPE).replace('torch.', '')}")

# --------------------------------------------- 2. 드라이브에 모델 캐시
from google.colab import drive
drive.mount("/content/drive")

from huggingface_hub import snapshot_download
repo = MODELS[MODEL_KEY]["repo"]
local = os.path.join(DRIVE_DIR, repo.replace("/", "__"))
first_time = not os.path.isdir(local) or not os.listdir(local)

print(f"\n■ 모델: {repo}")
print("  " + ("드라이브에 없습니다 — 내려받습니다 (처음 한 번만, 수 GB)"
              if first_time else f"드라이브에서 재사용: {local}"))
t0 = time.time()
snapshot_download(
    repo_id=repo,
    local_dir=local,
    # 안전텐서만. 중복 포맷(.bin, original/)을 받지 않아 드라이브 용량이 절약됩니다.
    allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "tokenizer*",
                    "*.py"],   # EXAONE 은 trust_remote_code 로 이 파일들을 읽습니다
)
size = sum(os.path.getsize(os.path.join(local, f)) for f in os.listdir(local)
           if os.path.isfile(os.path.join(local, f))) / 1e9
print(f"  준비 완료 · {size:.1f}GB · {time.time() - t0:.0f}초")

# ------------------------------------------------------------- 3. 모델 로드
from transformers import AutoModelForCausalLM, AutoTokenizer

print("\n■ 모델 로드 중…")
t0 = time.time()
import inspect
# transformers 가 최근 `torch_dtype` 을 `dtype` 으로 바꿨습니다. 설치된 쪽에
# 맞춰 골라야 구버전을 고정해 둔 환경에서도 로드됩니다.
DTYPE_KW = ("dtype" if "dtype" in
            inspect.signature(AutoModelForCausalLM.from_pretrained).parameters
            else "torch_dtype")
kwargs = {DTYPE_KW: DTYPE, "device_map": "cuda:0", "low_cpu_mem_usage": True}
if MODELS[MODEL_KEY]["load_4bit"]:
    from transformers import BitsAndBytesConfig
    kwargs["quantization_config"] = BitsAndBytesConfig(
        load_in_4bit=True, bnb_4bit_compute_dtype=DTYPE, bnb_4bit_quant_type="nf4")
    kwargs.pop(DTYPE_KW)

tok = AutoTokenizer.from_pretrained(local, trust_remote_code=True)
model = AutoModelForCausalLM.from_pretrained(local, trust_remote_code=True, **kwargs)
model.eval()
print(f"  로드 완료 · {time.time() - t0:.0f}초 · "
      f"VRAM {torch.cuda.memory_allocated() / 1e9:.1f}GB")

# --------------------------------------------------- 4. rustai 파이프라인
client = rustai.Client(
    providers=PROVIDERS,
    # 검색 폭. PyPI 0.2.0 의 이름은 `limit` 이고, 다음 릴리스에서 `max_results`
    # 로 바뀌면서도 계속 받습니다 -- 두 버전에서 다 도는 쪽을 씁니다.
    limit=MAX_RESULTS,
    max_tokens=CONTEXT_TOKENS,
    max_tokens_per_source=PER_SOURCE_CAP,
    contact_email=CONTACT_EMAIL,
    timeout=25.0,
)

LOOKUP = ("날씨", "기온", "미세먼지", "주가", "환율", "시세", "순위", "실시간",
          "지금", "현재", "오늘", "며칠", "몇 시")

# 실시간 수치용: 백과사전 없이.
live_client = rustai.Client(
    providers=[p for p in PROVIDERS if not p.startswith("wikipedia")],
    limit=MAX_RESULTS,
    max_tokens=CONTEXT_TOKENS,
    max_tokens_per_source=PER_SOURCE_CAP,
    contact_email=CONTACT_EMAIL,
    timeout=25.0,
)

def gather(question: str):
    """검색 → 읽기 → 정제 → 압축. 번호가 매겨진 근거와 계측치를 돌려줍니다."""
    # 백과사전은 "무엇인가"에 강하고 "지금 얼마인가"에 무력합니다. 그런데 그
    # 문서들은 길고 깨끗한 산문이라 슬리머의 예산을 독차지합니다 -- 측정해 보면
    # 날씨 사이트는 JS 로 그려져 36토큰을 내놓고, 엉뚱하게 걸려든 인물 문서는
    # 6,044토큰을 내놓습니다. 실시간 수치를 묻는 질문에서는 빼는 편이 낫습니다.
    lookup = any(k in question for k in LOOKUP)
    pipe = live_client if lookup else client
    t0 = time.time()
    res = pipe.research(question, max_sources=MAX_SOURCES)
    elapsed = time.time() - t0

    # context.selected 는 인덱스를 담습니다: source→articles, unit→그 글의 units.
    grouped = {}
    for sel in res.context.selected:
        grouped.setdefault(sel["source"], []).append(sel["unit"])

    blocks, cites = [], []
    for n, meta in enumerate(res.context.sources, start=1):
        units = sorted(grouped.get(meta["index"], []))
        if not units:
            continue
        art = res.articles[meta["index"]]
        body = "\n".join(art.units[u].markdown for u in units)
        weighted = sum(s["relevance"] * s["tokens"] for s in res.context.selected
                       if s["source"] == meta["index"])
        rel = weighted / max(meta["tokens"], 1)
        blocks.append(f"[{n}] {meta['title']}\n{body}")
        cites.append((n, meta["title"], meta["url"], meta["tokens"], rel))

    stamp = time.strftime("%Y-%m-%d %H:%M")
    context = f"(수집 시각: {stamp})\n\n" + "\n\n".join(blocks)
    return {
        "context": context,
        "cites": cites,
        "seconds": elapsed,
        # 실제로 프롬프트에 들어가는 문자열 기준. context.tokens 는 슬리머가
        # 고른 유닛 전체를 세므로, 여기서 재조립한 것과 조금 어긋납니다.
        "tokens": rustai.count_tokens(context),
        "considered": res.context.units_considered,
        "hits": len(res.results),
        "read": len(res.articles),
        "failures": res.failures,
        "lookup": lookup,
    }

# ------------------------------------------------------------- 5. 생성
import datetime
TODAY = datetime.date.today().isoformat()

GROUNDED = (
    f"오늘은 {TODAY} 입니다. 아래 근거는 방금 웹에서 수집한 것입니다.\n"
    "당신은 근거를 바탕으로 한국어로 답하는 조사 도우미입니다.\n"
    "- 근거에 있는 내용으로 답하고, 사실 문장 끝에 [1] 처럼 출처 번호를 다세요.\n"
    "- 근거가 질문과 무관하면 무관하다고 한 문장으로 말하세요.\n"
    "- 추측하지 마세요."
)
# 규칙은 셋입니다. 다섯이었을 때 이 크기의 모델은 답 대신 규칙을 따라 읽었고,
# "부산의 날씨는?"에 "부산의 날씨는 수집 시점 기준입니다."라고 답했습니다.
# 수집 시각을 덧붙이는 일은 모델이 아니라 코드가 합니다 -- 아래 gather() 참고.

PLAIN = "당신은 한국어로 답하는 도우미입니다. 아는 대로 간결하게 답하세요."

@torch.inference_mode()
def generate(messages) -> tuple[str, float, int]:
    text = tok.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
    ids = tok(text, return_tensors="pt").to(model.device)
    t0 = time.time()
    out = model.generate(
        **ids, max_new_tokens=MAX_NEW_TOKENS, do_sample=True,
        temperature=0.7, top_p=0.9, repetition_penalty=1.05,
        pad_token_id=(tok.pad_token_id if tok.pad_token_id is not None
                      else tok.eos_token_id),
    )
    new = out[0][ids["input_ids"].shape[1]:]
    return tok.decode(new, skip_special_tokens=True).strip(), time.time() - t0, len(new)

def rule(title=""):
    print("\n" + (f"── {title} " + "─" * max(0, 68 - len(title))) if title else "─" * 70)

# --------------------------------------------------------- 6. 대화 루프
history, last = [], None
print("\n" + "=" * 70)
print(f"  준비 완료 — {repo.split('/')[-1]} + rustai {rustai.__version__}")
print("  질문을 입력하세요.  /그만 종료 · /대조 근거없음비교 토글 · /출처 최근 출처")
print("=" * 70)

while True:
    try:
        q = input("\n질문> ").strip()
    except (EOFError, KeyboardInterrupt):
        print("\n종료합니다."); break
    if not q:
        continue
    if q in ("/그만", "/quit", "/exit"):
        print("종료합니다."); break
    if q == "/대조":
        COMPARE = not COMPARE
        print(f"근거 없는 답 비교: {'켜짐' if COMPARE else '꺼짐'}"); continue
    if q == "/출처":
        if last:
            for n, title, url, tk, rel in last["cites"]:
                print(f"  [{n}] {title[:58]}  ({tk}토큰, 관련도 {rel:.2f})\n      {url}")
        else:
            print("  아직 없습니다."); 
        continue

    # 후속 질문에만 직전 질문을 붙입니다. 글자 수로 재면 안 됩니다 -- 한국어는
    # 조밀해서 "딥러닝이 뭐야?"가 9글자이고, 길이로 판정하면 완결된 질문에
    # 엉뚱한 앞 질문이 붙어 검색이 통째로 빗나갑니다. 지시어가 있을 때만.
    ANAPHORA = ("그것", "그거", "그건", "이것", "이거", "이건", "저것", "저거",
                "거기", "그건가", "그럼", "그러면", "그래서", "방금", "위에서",
                "더 자세히", "왜 그런", "어떻게 그런")
    follow_up = bool(history) and any(a in q for a in ANAPHORA)
    query = f"{history[-1]['q']} {q}" if follow_up else q

    print(f"\n[수집 중] {query}")
    try:
        g = gather(query)
    except Exception as e:
        print(f"  수집 실패: {type(e).__name__}: {e}"); continue

    print(f"  히트 {g['hits']}개 → {g['read']}개 읽음 → "
          f"{g['considered']}개 블록 중 {g['tokens']}토큰 선별 · {g['seconds']:.1f}초"
          + ("  [실시간 질문 — 백과사전 제외]" if g["lookup"] else ""))
    if g["failures"]:
        for stage, msg in g["failures"][:3]:
            print(f"  · 건너뜀 {stage}: {msg[:60]}")
    if not g["cites"]:
        print("  쓸 만한 근거를 찾지 못했습니다."); continue

    if COMPARE:
        rule("근거 없이 (모델의 기억만)")
        plain, dt, n = generate([{"role": "system", "content": PLAIN},
                                 {"role": "user", "content": q}])
        print(plain)
        print(f"\n  ({dt:.1f}초 · {n}토큰 · {n/dt:.0f} tok/s)")

    rule("rustai 근거로")
    prompt = f"다음은 방금 웹에서 수집·정제한 근거입니다.\n\n{g['context']}\n\n질문: {q}"
    answer, dt, n = generate([{"role": "system", "content": GROUNDED},
                              *[m for h in history[-2:] for m in
                                ({"role": "user", "content": h["q"]},
                                 {"role": "assistant", "content": h["a"]})],
                              {"role": "user", "content": prompt}])
    print(answer)
    print(f"\n  ({dt:.1f}초 · {n}토큰 · {n/dt:.0f} tok/s)")
    print("\n  출처:")
    # 관련도는 *이 질문 안에서만* 의미가 있습니다. 질의가 다르면 척도가 달라져
    # 서로 비교할 수 없습니다 -- 측정해 보면 어떤 질문의 무관한 출처가 다른
    # 질문의 정확한 출처보다 높게 나옵니다. 거르는 데 쓰지 말고, 눈으로 보세요.
    for num, title, url, tk, rel in g["cites"]:
        print(f"   [{num}] {title[:58]}  ({tk}토큰, 관련도 {rel:.2f})\n       {url}")

    history.append({"q": q, "a": answer})
    last = g
    del g
    gc.collect()
