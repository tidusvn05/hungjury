# Cách dùng

## Chạy nhanh (không cần thư mục)

`hungjury` chạy được ở bất kỳ đâu — state gửi qua stdin/file, mọi dữ
liệu nằm trong thư mục global `~/.local/share/hungjury` (hoặc
`$HUNGJURY_HOME`):

```bash
hungjury decide --questions @questions.json --state-file ticket.txt
```

## Dùng trong project — `.hungjury/`

Với một project thật, tạo thư mục `.hungjury/` (giống `.git`) để giữ
config, policy và memory **riêng cho project đó**:

```bash
cd my-project && hungjury init
# → .hungjury/config.toml  (skeleton, mọi thứ đã comment sẵn)
# → .hungjury/policy.md    (rubric của bạn — tự động inject vào prompt)
# → .hungjury/.gitignore   (memory.db, cache/, calls.jsonl, state.json)
```

Từ đó mọi lệnh `hungjury` chạy **ở bất kỳ subdir nào** của project đều
walk-up tìm `.hungjury/` gần nhất — memory.db, cache, quota đều local,
không lẫn với project khác. `hungjury doctor` cho thấy root được chọn.

Quy tắc precedence: `--memory-db` > `$HUNGJURY_HOME` > `.hungjury/` >
global. `hungjury.toml` ở ancestor cũng được walk-up nhưng chỉ nạp
config — không di chuyển memory (backward compatible).

## Tách memory theo mục đích

Hai cấp độ:

```toml
# .hungjury/config.toml
namespace = "triage"            # mềm: scopes thành triage:q:<qid>,
                               # tách trong cùng một db

[profiles.review]               # cứng: db riêng hoàn toàn
jurors    = ["codex:gpt-5.6-terra@low"]
memory_db = "memory-review.db"  # resolve theo .hungjury/
policy_file = "policy-review.md"
```

`memory stats` liệu kê breakdown theo namespace.

**Cách chọn tách biệt**: một project, một mục đích → `.hungjury/` +
`namespace`; một project nhiều mục đích → `[profiles.X]` với `memory_db`
riêng. Luôn viết `policy.md` trước khi bật escalate — benchmark
(`docs/BENCHMARK.md`) cho thấy judge lệch policy gây −17pts.

`questions.json`:

```json
{
  "department": {
    "type": "choice",
    "id": "support.department",
    "instructions": "Which team should handle this",
    "criteria": {
      "billing": "Payment or subscription issues",
      "technical": "Bugs or integration problems",
      "sales": "Pricing or account questions"
    }
  },
  "frustration": {
    "type": "score",
    "instructions": "How frustrated the customer appears",
    "criteria": ["Calm, just stating facts", "Frustrated but civil", "Very angry, strong language"]
  },
  "is_urgent": {"type": "noul", "instructions": "The message conveys urgency or time-sensitivity"}
}
```

Kết quả (stdout, rút gọn):

```json
{
  "id": "dec_01J…",
  "decided_by": "jury",
  "answers": {
    "department":  {"type": "choice", "choice": "technical",
                    "probabilities": {"billing": 0.0, "technical": 1.0, "sales": 0.0}, "confidence": 1.0},
    "frustration": {"type": "score", "score": 1.33, "legend": "Frustrated but civil", "confidence": 0.53},
    "is_urgent":   {"type": "noul", "noul": 1.0, "confidence": 1.0}
  },
  "hung": [],
  "sources": {"department": "jury", "frustration": "jury", "is_urgent": "judge"},
  "memory": {"rulings": 2, "precedents": 1, "facts": 0},
  "usage": {"wall_ms": 7421, "jurors": [{"juror": "claude:haiku", "status": "ok", "ms": 6300}]}
}
```

`state` cũng có thể là một workspace để agent tự khám phá (chỉ đọc):

```bash
hungjury decide --questions @pr-questions.json --workspace ./my-repo --hint "Xem diff của nhánh hiện tại so với main"
```

**Exit code:** `0` = có quyết định, `2` = jury treo chưa được xử (dùng được ngay trong shell/CI để chuyển cho người), `1` = lỗi.

Các lệnh khác: `hungjury feedback` (người sửa đáp án), `hungjury learn` (model cao chấm lại ngoài luồng), `hungjury eval` (đo accuracy/latency theo từng cấu hình), `hungjury memory search|show|forget|export|import|merge`, `hungjury doctor`.

## Vận hành hằng ngày

```bash
# Chạy nhiều case một lần (song song theo limits.max_concurrency)
hungjury batch cases.jsonl --out results.jsonl

# Gom N case vào một call mỗi juror — ít calls hơn hẳn, vote vẫn per-item
hungjury batch cases.jsonl --out results.jsonl --pack 10

# Gắn rubric/policy của domain vào cả juror lẫn judge (đổi policy ⇒ cache tự invalidate)
hungjury --policy-file triage-policy.md decide req.json

# Kiểm tra memory và lịch sử quyết định
hungjury memory list --kind ruling --all          # mọi status, kể cả contested
hungjury memory decisions --last 20               # quyết định gần nhất + ai quyết
hungjury memory stats                             # entries, nguồn, quota, juror stats
hungjury memory resolve <entry-id> --accept       # contested → active (--reject → forgotten)

# Học ngoài luồng: judge chấm lại các quyết định gần nhất thay vì chỉ hàng hung
hungjury learn --audit --recent 20
hungjury feedback <decision-id> --set dept=technical --note "label sai"

# Soi rubric trước khi chạy (miễn phí, không tốn call)
hungjury lint questions.json        # cảnh báo: level trừu tượng, boundary một chiều, criteria chồng lấn

# Đo trên dữ liệu thật — nhiều seed một lệnh, memory db tạm (không đụng db thật)
hungjury eval cases.jsonl --seeds 3 --label triage --audit-train 8
```

Khi người hoặc audit phủ định một quyết định jury đã *decided* (không hung), các ruling/precedent do judge ghi cho câu hỏi đó bị đánh `contested` — vẫn xem được bằng `memory list --all` / `memory review`, không còn được inject vào prompt. Đây là guard chống memory lan truyền lỗi của judge (xem `docs/BENCHMARK.md`).

## Tin cậy & vòng đời rulings

- **`min_quorum`** (mặc định 2): câu hỏi có ít hơn quorum ballot hợp lệ (juror timeout/lỗi/abstain) được coi là *hung* — một juror sống sót duy nhất không được âm thầm quyết định.
- **Score bimodal hung**: confidence của score = `min(1 − normalized_stddev, bucket_support)` — share phiếu chọn đúng level được báo. Phiếu {0, 2, 2} trên thang 0–2 báo legend "1" không ai chọn → support 0 → hung, thay vì mean 1.33 âm thầm quyết.
- **Provisional rulings**: ruling mà judge rút ra từ một câu jury *không quyết được* (hung/quorum) ghi với `trust = memory.provisional_trust` (mặc định 0.4, thấp hơn judge thường 0.8). Nó chỉ được nâng lên full trust khi `feedback` hoặc `learn --audit` sau đó *tái xác nhận* verdict của judge trên scope đó.
- **`supersedes`**: rulings trong prompt judge có gắn `[id:…]`; judge có thể retire ruling cũ khi viết ruling mới (`"supersedes": "<id-prefix>"`).
- **`memory.ruling_ttl_days`** (mặc định 0 = tắt): rulings `active` quá N ngày tự thành `stale` mỗi lần `learn` chạy.
- **`memory review`** liệt kê contested entries chờ duyệt; `memory resolve <id> --accept|--reject` để xử lý.

## Chi phí

`[costs]` trong config map backend → USD/call; response `usage.est_cost_usd` và eval `est_cost_usd` ước tính theo số call thật (gồm retry):

```toml
[costs]
claude = 0.08
codex = 0.05
devin  = 0.05
```

## SDK

Python — `sdk/python/hungjury.py`, wrapper mỏng không dependency, gọi
binary qua stdin:

```python
from hungjury import system_one, feedback

d = system_one(
    "I was charged twice, refund please — this blocks payroll.",
    {"dept": {"type": "choice", "id": "support.dept",
              "instructions": "which team",
              "criteria": {"billing": "payment/refund", "technical": "bugs"}},
     "urgent": {"type": "noul", "id": "support.urgent",
                "instructions": "time-sensitive"}},
)
if not d.ok:            # exit_code == 2 → hung, route to a human
    route_to_human(d)
print(d.answers)        # {"dept": {"choice": "billing", ...}, ...}
feedback(d.id, {"dept": "billing"}, note="correct")
```

TypeScript: `sdk/typescript/` — zero-dep Node wrapper cùng hình dạng
(`decide`/`systemOne`/`feedback`, `cwd` cho project discovery).
