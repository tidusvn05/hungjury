# Examples — runnable spikes

Mỗi thư mục con là một project `.hungjury/` tự chứa: config, `policy.md`,
memory.db riêng — memory của các use case không bao giờ lẫn nhau.

## support-triage — phân luồng ticket hàng loạt

```bash
cd examples/support-triage
hungjury batch cases.jsonl --out results.jsonl   # 20 tickets, song song
python3 ../score.py cases.jsonl results.jsonl    # chấm theo expected labels
hungjury memory decisions --last 20              # ai quyết, hung nào
```

`cases.jsonl` có `expected` labels viết tay (gồm mixed-signal,
courtesy-urgency trap, và một case cố tình mơ hồ). `score.py` so từng
key: `"hung"` trong expected = đúng khi key đó nằm trong `hung`.

**Kết quả đo được (claude:haiku + codex + devin):** 56/60 = 93%; 1 case
hung → `queue_pending` (t19 "chargeback today" — jurors chia 2:1 vì nó
vừa là deadline vừa là hăm dọa: đúng chỗ policy cần con người).

**Eval arms trên use case này** (`hungjury eval cases.jsonl`, 10 train /
10 test — `eval-*.json`), 3 runs (non-deterministic agents ⇒ cùng seed
cũng dao động ±7pts ở n=10):

| arm | run cũ | seed 1 | seed 7 | mean |
|---|---|---|---|---|
| jury | 93.3% | 100% | 93.3% | ~95.6% |
| jury+memory | 100% | 96.7% | 90.0% | ~95.6% |
| judge cold | 96.6% | 93.1% | 93.3% | ~94.3% |
| judge_informed | 93.1% | 96.6% | 93.3% | ~94.3% |

Kết luận trung thực: ở n=10 mọi chênh lệch nằm trong noise. Điều *đúng*
là memory **không còn poison** ở use case này (policy.md align sẵn —
khác adversarial bench), còn "memory giúp 100%" của run đầu là
single-run fluke. Cần bộ case lớn hơn (~60+) mới phân biệt được jury vs
judge vs +memory có ý nghĩa thống kê.

**Đã đo ở n=90** (`hungjury eval ../../bench/adversarial/cases.jsonl` —
60 cases viết để split jury, cùng qids/policy — `eval-adv-*.json`):

| arm | seed 1 | seed 7 | mean |
|---|---|---|---|
| jury | 86.7% | 82.2% | 84.4% |
| **jury+memory** | **90.0%** | **85.6%** | **87.8%** |
| judge cold | 91.1% | 85.6% | 88.3% |
| judge_informed | 91.1% | 87.8% | 89.4% |

**Memory đóng 75–100% gap jury→judge** (+3.3pts cả 2 seeds,
`go.pass=true` cả hai) — rulings từ train pass nâng jury gần ngang
judge. Đây là bằng chứng đầu tiên *ổn định* rằng memory giúp khi
policy-aligned. Lỗi còn lại tập trung ở `frustration` (score off-by-one
chiếm ~80% mismatches) — score calibration là điểm yếu kế tiếp.

## pr-review — cổng review trên workspace

```bash
cd examples/pr-review
hungjury batch cases.jsonl --out results.jsonl   # 4 commit thật của repo này
python3 ../score.py cases.jsonl results.jsonl
```

`state` là `{"workspace": "../..", "hint": "git show <commit>…"}` —
juror tự mở diff. `escalate=sync`: mọi case đều qua judge vì jurors chia
phiếu trên `breaking`/`risk`; rulings lưu theo `q:pr.*` + fact theo
`ws:<repo>` (key treo được ghi `trust=0.4` provisional như thiết kế).

**Kết quả:** 9/12 theo expected labels — nhưng 2 "miss" là judge áp
policy *chính xác hơn* labels: `068b7ad` có schema migration →
`breaking=true` đúng theo policy, label tay của tôi sai. Bài học: khi
judge lệch expected, xem lại policy trước khi xem lại judge.

## log-triage — phân loại lỗi CI

```bash
cd examples/log-triage
hungjury batch cases.jsonl --out results.jsonl   # 8 log fixtures
python3 ../score.py cases.jsonl results.jsonl
# hoặc pipe trực tiếp (`--state-file -` = stdin):
tail -50 build.log | hungjury decide --state-file - --questions @questions.json
```

**Kết quả:** 24/24 = 100% — flaky vs actionable phân biệt đúng hết
(timeout/OOM/503 → retry; assert/compile/migration → dev fix).

## Bài học từ spike

- **Hung bắt *bất đồng*, `"abstain"` bắt *thiếu thông tin*.** t12
  ("hello?? anyone there") từng bị 3 juror đồng loạt đoán `technical`
  conf=1.0. Nay juror có thể trả `"abstain"` (mọi kiểu câu hỏi) — cả 3
  abstain → `department` hung, `sources` chỉ ghi nhận keys đã quyết.
  Không cần thêm option `"unknown"` thủ công nữa.
- **Cache đúng và nhanh**: rerun 20 cases = 23ms, answers identical,
  `decided_by=cache`. Đổi `policy.md` → cache key đổi → invalidate tự động.
- **Memory chỉ ghi khi judge chạy**: jury-decided cases không ghi
  rulings (đúng thiết kế) — `entries` chỉ xuất hiện trong db của
  pr-review nơi escalation xảy ra.

## Dọn sạch

```bash
rm -f examples/*/.hungjury/memory.db examples/*/.hungjury/calls.jsonl
rm -rf examples/*/.hungjury/cache examples/*/.hungjury/state.json
```
