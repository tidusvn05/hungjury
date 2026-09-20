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
judge. Lỗi còn lại tập trung ở `frustration` (score off-by-one
chiếm ~80% mismatches).

**Self-improve loop: 4 vòng đo+fix rubric** (`eval-adv-rubric*.json`):

| vòng | thay đổi | jury | jury+mem | judge | informed |
|---|---|---|---|---|---|
| s3 | levels có marker words | 96.7% | 93.3% | 93.3% | 93.3% |
| s4 | "deadline ≠ penalty" | 97.8% | 98.9% | 98.9% | 100% |
| s5 | (confirm) | 100% | 96.7% | 97.8% | 97.8% |
| s6 | rulings phụ thuộc policy | 97.8% | **100%** | 100% | 100% |
| s8 | (confirm) | 95.6% | 95.6% | 100% | 100% |

Bài học:
1. **Rubric levels mơ hồ là bottleneck** — "Very angry, strong
   language" không bắt được "Unacceptable, honestly". Ghi marker words
   vào `criteria` từng level nâng jury +13pts (84→97%).
2. **Boundary phải nói cả hai chiều**: "names a penalty landing if
   unresolved" (chargeback, legal obligation, staff idle) vs "deadline
   alone = urgency not frustration" — chỉ nói một chiều thì model
   over-trigger sang deadline cases.
3. **Memory lan cả lỗi hệ thống của judge** — khi judge overshoot
   (s3/s5), rulings kéo jury xuống. Header memory giờ ghi "on conflict
   the policy wins" → jury+mem về 100% (s6) vì rulings chỉ sửa chỗ
   jury thiếu, không đè policy.
4. **Memory chỉ engage khi escalation fire** — s8: train pass 0 hung →
   0 rulings/precedents → `memory_injected=0` → jury+mem ≡ jury. Memory
   là *conditional benefit*: có giá trị khi rubric yếu/case khó làm jury
   treo; khi rubric tốt jury đã ~98% thì không còn headroom.
5. **Residual ~1-4% là label-boundary noise** — "Blocking our launch
   tomorrow" vừa là deadline vừa là impact; generator label theo slot,
   semantics thì nhập nhằng. Không nên siết rubric thêm (overfit).

`eval` report giờ có `memory_injected` (số entries đã inject per arm)
và `go.memory_delta_vs_jury`; `go.pass` đúng cả khi judge không phải
ceiling (gap ≤ 0 → pass ⇔ memory không kéo jury xuống).

**Phép đo cuối — 64 cases × 3 seeds, `--audit-train 8`**
(`eval-adv64.json`; set giờ gồm 4 information-free cases với
`expected: hung` trên `department`):

| arm | mean | min–max | memory_injected |
|---|---|---|---|
| jury | 95.7% | 93.6–96.8% | 0 |
| **jury+memory** | **98.6%** | **97.9–100%** | ~220 |
| judge cold | 100% | — | 0 |
| judge_informed | 100% | — | 0 |

`go_pass 3/3`, `memory_delta_vs_jury` mean **+2.9pts**,
`gap_closed` 0.67–1.0. Ba điểm đáng chú ý:

1. **`--audit-train` giải quyết triệt để bài toán memory-engagement**
   (bài học 4): mỗi seed đều có rulings inject (~220 entries) kể cả khi
   train pass 0 hung — audit re-judge jury decisions, judge tự emit
   rulings/precedents ở full trust vì jury đã quyết. Không còn run nào
   memory no-op.
2. **Abstention đúng cả khi có memory**: mọi arm giữ `department` hung
   trên empty tickets — rulings không đè abstention.
3. **Judge 100% cả 3 seeds** — rubric/policy đã hội tụ trên
   distribution của generator; misses còn lại của jury toàn ở
   `frustration` off-by-one (per_key ~80–94% vs 100% các key khác).

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

## spam-filter — phân loại mail

```bash
cd examples/spam-filter
hungjury batch cases.jsonl --out results.jsonl   # 12 emails viết tay
python3 ../score.py cases.jsonl results.jsonl
```

Câu hỏi: `verdict` choice (ham/promo/phishing/scam — policy áp
precedence "check phishing/scam markers trước"), `credential_risk`
noul (ask credentials/card/OTP — wire fee là scam, *không* tính),
`spam_score` score 0–2.

**Kết quả:** 35/36 = 97%. Adversarial đúng hết: password-zip invoice
→ phishing; "won $1M, wire $50 fee" → scam + credential_risk=false.
Miss: mail `"?"` — jury abstain cả `spam_score` dù "không có spam
signal → 0" là defensible default (over-abstention trên score/noul
cho empty content — pattern lặp lại, xem r11/m9 notes).

## support-routing — điều phối queue

```bash
cd examples/support-routing
hungjury batch cases.jsonl --out results.jsonl   # 12 tickets
python3 ../score.py cases.jsonl results.jsonl
```

Câu hỏi: `queue` choice với precedence legal>manager>billing>sales>
technical (multi-intent tickets), `priority` score 0–2, `vip` noul
(chỉ enterprise signals rõ ràng — "I'm a paying customer" không tính).

**Kết quả:** 30/36 = 83%; `queue` **12/12** (kể cả legal>billing
precedence và "bug or plan limit?" → sales). Misses là bài học labels:
r1 "not urgent" refund → priority 0 defensible (label 1 quá tay); r5
manager-escalation → 1 (rubric: critical = outage/legal — label 2
vượt rubric); r3 "500 seats evaluating" → `vip` hung vì nó *đúng* là
enterprise signal. Khi jury lệch expected → check labels trước.

## content-moderation — kiểm duyệt UGC

```bash
cd examples/content-moderation
hungjury batch cases.jsonl --out results.jsonl   # 10 content items
python3 ../score.py cases.jsonl results.jsonl
```

Câu hỏi: `action` choice (allow/warn/remove/escalate_human theo
precedence illegal/safety > remove > warn > allow), `severity`
score 0–2, `illegal_or_safety` noul.

**Kết quả:** 26/30 = 87%; `severity` 10/10. Ba `action` hung đều là
borderline *thật*: m3 "kill yourself" (remove vs escalate tuỳ
credible-threat), m2 profanity+refund, m10 "dumb take lol" — jury chia
đúng chỗ policy mơ hồ, escalate=queue đưa về cho người. Đó là hành vi
được thiết kế, không phải lỗi: hung trên case tranh chấp thật có giá
trị hơn một quyết định tự tin nhưng ngẫu nhiên.

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
