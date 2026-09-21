# hungjury

> Quyết định có kiểu (typed decisions) từ một "bồi thẩm đoàn" gồm các agent CLI chạy headless — có trí nhớ án lệ.

**Trạng thái:** [v0.1.0 đã release](https://github.com/tidusvn05/hungjury/releases) — binaries cho linux/macos/windows. Kế hoạch triển khai: [docs/PLAN.md](docs/PLAN.md).

## Ý tưởng

`hungjury` là một CLI viết bằng Rust. Nó nhận vào một **trạng thái phi cấu trúc** (`state`) và một tập **câu hỏi có kiểu** (`questions`), rồi trả về JSON gồm các **quyết định có kiểu kèm xác suất** mà phần mềm dùng trực tiếp được — không cần parse text.

Phần xử lý không gọi LLM API, mà chạy các **agent CLI ở chế độ headless** (`claude -p`, `codex exec`, `devin -p`) như subprocess. Nhiều agent cùng cân nhắc rồi bỏ phiếu; tỉ lệ phiếu chính là phân phối xác suất.

Tên gọi: *hung jury* là bồi thẩm đoàn không thống nhất được phán quyết — đúng với tình huống các agent bất đồng, tức **confidence thấp**. Khi đó vụ việc được chuyển lên **thẩm phán** (model cấp cao), và phán quyết của thẩm phán trở thành **án lệ** cho các lần sau.

### Ba kiểu câu hỏi

| Kiểu | Ý nghĩa | Mỗi juror trả | Kết quả gộp |
|---|---|---|---|
| `choice` | Chọn 1 trong N lựa chọn | một lựa chọn | Lựa chọn thắng + `probabilities` + `confidence` |
| `score` | Chấm điểm theo thang đo có mô tả từng mức | một mức (số nguyên) | Điểm trung bình (số thực) + `legend` + `confidence` |
| `noul` | Mệnh đề đúng/sai | `true` / `false` | Tỉ lệ phiếu `true`, trong [0, 1] + `confidence` |

Mọi kiểu đều chấp nhận thêm `"abstain"`: juror từ chối khi state thiếu
thông tin — abstain tính như *không có ballot*, nên dưới `min_quorum`
câu hỏi treo thay vì bị đoán. (Hung bắt *bất đồng*; abstain bắt *thiếu
thông tin* — đóng kịch bản "unanimous confident guess".)

## Nguồn cảm hứng

Lấy cảm hứng từ [Jev của TypeSafe AI](https://typesafe.ai/blog/introducing-system-one-models-and-jev) — một "System One Model": model tối ưu cho tự động hóa thay vì chat, trả về quyết định có kiểu kèm xác suất đã calibrate, độ trễ 70–500ms. Hình dạng request/response của `hungjury` cố ý bám theo `typesafe_sdk` ([docs](https://docs.typesafe.ai/)).

## Định vị: System 2 sau giao diện System 1

`hungjury` **không phải** "Jev tự làm". Nó đánh đổi đúng thứ Jev bán, nên cần điểm mạnh khác.

### Mất gì

- **Tốc độ:** sàn đo được là **~5–9 giây** cho một lần gọi agent CLI, so với 70–500ms của Jev. Chỉ hợp với batch, CI, triage và workflow bất đồng bộ — không hợp real-time.
- **Logprobs:** CLI không trả về xác suất của token, nên không có phân phối xác suất thật từ model.
- **Đảm bảo kiểu dữ liệu:** chỉ làm được "validate theo schema + retry + báo lỗi có kiểu", không có đảm bảo toán học như Jev tuyên bố.

### Được gì

- **`state` không cần là chuỗi:** có thể là một repo hay thư mục. Agent tự đọc file, grep rồi mới trả lời. Ví dụ `noul("PR này có breaking change")` — model một-lần-gọi không điều tra được, agent thì làm được.
- **Bản chất:** tư duy System 2 (chậm, có điều tra) đứng sau giao diện System 1 (quyết định có kiểu, dùng ngay được trong code).
- **Chi phí:** chạy bằng gói thuê bao sẵn có (Claude, ChatGPT, Devin) thay vì trả tiền API.
- **Trí nhớ:** kiến thức của model cấp cao được lưu lại và dùng cho model cấp thấp ở những lần sau.

> **Lưu ý điều khoản dịch vụ:** dùng gói thuê bao để tự động hóa cho cá nhân thì được; đừng xây dịch vụ bán cho người khác trên nền gói thuê bao.

## Cách dùng nhanh

```bash
# Quyết một case
hungjury decide --questions @questions.json --state-file ticket.txt
# → JSON: answers (typed + probabilities + confidence), hung, sources

# Trong project: .hungjury/ giữ config + policy + memory riêng (giống .git)
cd my-project && hungjury init

# Nhiều case: song song theo limits.max_concurrency
hungjury batch cases.jsonl --out results.jsonl

# Hoặc gom N case vào MỘT call mỗi juror (prompt batching) —
# vote/hung/escalation vẫn per-item, calls giảm ~N lần
hungjury batch cases.jsonl --out results.jsonl --pack 10
```

Exit code `0` decided / `2` hung (route cho người) / `1` lỗi. Hướng dẫn
đầy đủ — profiles, namespace, memory, feedback/learn/eval, chi phí, SDK
Python/TypeScript: **[docs/USAGE.md](docs/USAGE.md)**.

## Số đo thực tế

Tóm tắt — bảng đầy đủ và cách reproduce ở
[docs/BENCHMARK.md](docs/BENCHMARK.md):

- **Latency sàn** ~5–9s/call bất kể backend (CLI spawn + roundtrip).
- **Chọn model juror:** `terra`/`luna`/`sol` ngang nhau trong nhiễu
  (90–92%) → mặc định `codex:gpt-5.6-terra@low`; tier cao dành cho judge.
- **Throughput:** `devin:swe-2-medium` nhanh nhất (~1.0s/case @batch50,
  `max_concurrency=6`); `batch` nhanh hơn `decide` tuần tự ~5.7×.
- **`batch --pack 12`** (12 emails, all-devin jury): 94% accuracy trong
  **9.2s / 3 calls** vs 33s / 36 calls khi không pack — cùng accuracy,
  12× ít calls.
- **Memory đa domain** (~60 case × 7 use case): jury ensemble ≥ judge đơn
  trên 5/7 domain; memory giúp khi rulings của judge khớp policy, hại khi
  judge lệch policy (−17pts — fix bằng `--policy-file` cho judge).

## Use cases thực tế

Bảy kịch bản runnable trong `examples/` — mỗi cái là một project
`.hungjury/` tự chứa (config + policy + memory riêng):

| Example | State | Câu hỏi | Đo được |
|---|---|---|---|
| [`support-triage`](examples/support-triage) | ticket text | choice dept + score frustration + noul urgent | 93% |
| [`pr-review`](examples/pr-review) | **workspace** (repo) | noul review/breaking + score risk | agent tự đọc diff; rulings ghi `ws:` scope |
| [`log-triage`](examples/log-triage) | log CI/production | noul flaky/actionable + score severity | 100% |
| [`spam-filter`](examples/spam-filter) | raw email | choice verdict + noul credential_risk + score | 97% |
| [`support-routing`](examples/support-routing) | ticket text | choice queue + score priority + noul vip | 83% (misses = label vượt rubric) |
| [`content-moderation`](examples/content-moderation) | user content | choice action + score severity + noul safety | 87% (hung đúng chỗ borderline) |
| [`email-classification`](examples/email-classification) | raw email | choice folder + noul action + score priority | 94%, jury all-devin |

Chi tiết chạy từng use case + phân tích misses: [examples/README.md](examples/README.md).

### Khi nào nên/không nên dùng

Nên dùng khi câu trả lời **mơ hồ nhưng có rubric**, cần tín hiệu xác suất (confidence/probabilities) và hung là output hợp lệ — triage, gate, enrich. Không dùng cho câu hỏi khách quan chắc chắn (regex/parse được thì code thẳng rẻ hơn) hoặc khi mỗi quyết định sai đều không chấp nhận được mà không có người duyệt.

## Thiết kế tóm tắt

```
decide → cache → tra memory (Rust, <10ms) → juror cấp THẤP song song → bỏ phiếu
                                                    │
                                  jury treo? ── có ─┴→ judge cấp CAO phán
                                                         → ghi ruling / precedent / fact
```

- Xác suất đến từ **bỏ phiếu**, không phải model tự khai.
- Kiến thức gắn vào **câu hỏi** (ruling/precedent) hoặc **workspace**
  (fact kèm evidence hash); jury cấp thấp không bao giờ tự ghi memory.
- Memory là SQLite/FTS5 local, export/import/merge bằng file;
  contested entries không ghi đè âm thầm.
- `state` text → agent không tool; `state` workspace → chỉ tool đọc;
  env API key bị loại khỏi subprocess.

Chi tiết đầy đủ: [docs/DESIGN.md](docs/DESIGN.md).

## Quyết định còn mở

- [ ] Memory có thật sự kéo được model thấp lên gần model cao không — trả lời bằng thí nghiệm `eval` ở Phase 2 (mốc go/no-go, xem PLAN).
- [x] Model juror mặc định cho devin — chốt `swe-2-medium`: free, latency thấp nhất trong spike (~1.0s/case @concurrency 6), khác họ model với claude/codex ⇒ phiếu độc lập hơn.
- [ ] Cấu hình tool chỉ-đọc cho `claude -p` khi không bypass permission — cần kiểm ở Phase 0.
- [ ] Tách lớp backend dùng chung với agentwiki thành crate riêng (sau khi API ổn định).

Các mục mở trước đây (ngôn ngữ, flag headless, công thức confidence, trọng số juror) đã chốt — xem bảng ở [docs/PLAN.md §9](docs/PLAN.md).

## Tài liệu

| File | Nội dung |
|---|---|
| [docs/USAGE.md](docs/USAGE.md) | Cách dùng đầy đủ: init, profiles, memory, daily ops, SDK |
| [docs/DESIGN.md](docs/DESIGN.md) | Kiến trúc: adapters, voting, memory, an toàn |
| [docs/BENCHMARK.md](docs/BENCHMARK.md) | Benchmark đa domain + microbenchmarks (latency, throughput, pack) |
| [docs/PLAN.md](docs/PLAN.md) | Kế hoạch phases và các quyết định đã chốt |
| [examples/README.md](examples/README.md) | Hướng dẫn chạy 7 use case + bài học từ spike |
| [CHANGELOG.md](CHANGELOG.md) | Release notes (keepachangelog) |
