# hungjury

> Quyết định có kiểu (typed decisions) từ một "bồi thẩm đoàn" gồm các agent CLI chạy headless — có trí nhớ án lệ.

**Trạng thái:** bản thiết kế — chưa có code. Kế hoạch triển khai chi tiết: [docs/PLAN.md](docs/PLAN.md).

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

## Nguồn cảm hứng

Lấy cảm hứng từ [Jev của TypeSafe AI](https://typesafe.ai/blog/introducing-system-one-models-and-jev) — một "System One Model": model tối ưu cho tự động hóa thay vì chat, trả về quyết định có kiểu kèm xác suất đã calibrate, độ trễ 70–500ms. Hình dạng request/response của `hungjury` cố ý bám theo `typesafe_sdk` ([docs](https://docs.typesafe.ai/)).

## Định vị: System 2 sau giao diện System 1

`hungjury` **không phải** "Jev tự làm". Nó đánh đổi đúng thứ Jev bán, nên cần điểm mạnh khác.

### Mất gì

- **Tốc độ:** sàn đo được là **~6–9 giây** cho một lần gọi agent CLI với prompt nhỏ (xem bảng dưới), so với 70–500ms của Jev. Chỉ hợp với batch, CI, triage và workflow bất đồng bộ — không hợp real-time.
- **Logprobs:** CLI không trả về xác suất của token, nên không có phân phối xác suất thật từ model.
- **Đảm bảo kiểu dữ liệu:** chỉ làm được "validate theo schema + retry + báo lỗi có kiểu", không có đảm bảo toán học như Jev tuyên bố.

### Được gì

- **`state` không cần là chuỗi:** có thể là một repo hay thư mục. Agent tự đọc file, grep rồi mới trả lời. Ví dụ `noul("PR này có breaking change")` — model một-lần-gọi không điều tra được, agent thì làm được.
- **Bản chất:** tư duy System 2 (chậm, có điều tra) đứng sau giao diện System 1 (quyết định có kiểu, dùng ngay được trong code).
- **Chi phí:** chạy bằng gói thuê bao sẵn có (Claude, ChatGPT, Devin) thay vì trả tiền API.
- **Trí nhớ:** kiến thức của model cấp cao được lưu lại và dùng cho model cấp thấp ở những lần sau.

> **Lưu ý điều khoản dịch vụ:** dùng gói thuê bao để tự động hóa cho cá nhân thì được; đừng xây dịch vụ bán cho người khác trên nền gói thuê bao.

### Số đo thực tế (2026-09-19)

Prompt phân loại nhỏ, có schema, ba CLI chạy song song:

| Juror | Wall-clock |
|---|---|
| `devin -p` (model mặc định) | 6.1s |
| `claude -p --model haiku`, tắt tools + system prompt tối giản | 6.3s |
| `claude -p --model haiku`, mặc định | 7.4s |
| `codex exec`, `gpt-5.6-sol` effort low | 8.6s |

Chạy song song thì thời gian chờ bằng juror chậm nhất. Memory không hạ được sàn này; nó giúp tốc độ bằng cách **giảm số lần phải leo thang lên model cao** và **giảm thời gian agent điều tra repo**.

Benchmark đa domain (~60 case/use case × 7): [`docs/BENCHMARK.md`](docs/BENCHMARK.md), dataset + report trong [`bench/`](bench/).

## Cách dùng dự kiến

```bash
hungjury decide -q questions.json --state-file ticket.txt
```

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
  "memory": {"rulings": 2, "precedents": 1, "facts": 0},
  "usage": {"wall_ms": 7421, "jurors": [{"juror": "claude:haiku", "status": "ok", "ms": 6300}]}
}
```

`state` cũng có thể là một workspace để agent tự khám phá (chỉ đọc):

```bash
hungjury decide -q pr-questions.json --workspace ./my-repo --hint "Xem diff của nhánh hiện tại so với main"
```

**Exit code:** `0` = có quyết định, `2` = jury treo chưa được xử (dùng được ngay trong shell/CI để chuyển cho người), `1` = lỗi.

Các lệnh khác: `hungjury feedback` (người sửa đáp án), `hungjury learn` (model cao chấm lại ngoài luồng), `hungjury eval` (đo accuracy/latency theo từng cấu hình), `hungjury memory search|show|forget|export|import|merge`, `hungjury doctor`.

SDK Python/TypeScript (giữ hình dạng `system_one(state, questions)`) sẽ là lớp bọc mỏng gọi binary — làm sau.

## Thiết kế

### 1. Adapter cho từng CLI

Ba backend: **claude**, **codex**, **devin**; model viết dạng `<backend>:<model>[@<effort>]`, ví dụ `codex:gpt-5.6-terra@low`. Lớp backend tái sử dụng từ project [agentwiki](https://github.com/tidusvn05/agentwiki).

- Chạy CLI như subprocess headless; loại bỏ mọi env API key để không vô tình chuyển sang billing API.
- Ép output theo JSON schema sinh từ `questions`: claude dùng `--json-schema`, codex dùng `--output-schema` (cả hai đã chạy thử); devin không có flag schema nên ép bằng prompt.
- Luôn validate lại. Sai schema → retry có giới hạn kèm phản hồi lỗi → hết lượt thì juror đó bị loại khỏi phiếu. Không bao giờ trả dữ liệu sai kiểu.

### 2. Xác suất đến từ bỏ phiếu, không phải tự khai

- **Không** để model tự khai xác suất — con số đó calibrate kém.
- Chạy nhiều juror và/hoặc nhiều sample; **tỉ lệ phiếu là phân phối xác suất**.
- **Mức bất đồng là confidence:** `choice` = chênh lệch giữa hai lựa chọn đầu; `score` = 1 − độ lệch chuẩn đã chuẩn hoá; `noul` = |2p − 1|. Một phiếu duy nhất thì `confidence = null`.
- Jury "treo" khi confidence dưới ngưỡng (mặc định 0.5; với 3 juror, 2–1 là treo).

### 3. Hai tầng model và trí nhớ án lệ

```
decide → cache → tra memory (Rust, <10ms) → juror cấp THẤP chạy song song → bỏ phiếu
                                                       │
                                     jury treo? ── có ─┴→ judge cấp CAO phán
                                                            → ghi ruling / precedent / fact vào memory
```

Mấu chốt: `state` mỗi lần một khác, nhưng `questions` nằm cố định trong code người gọi. Vì vậy kiến thức được **gắn vào câu hỏi**, không gắn vào state:

| Loại | Gắn vào | Nội dung |
|---|---|---|
| **ruling** | câu hỏi | Quy tắc diễn giải tổng quát do model cao chưng cất, vd "lỗi tích hợp → technical; chỉ billing khi nói về tiền bị trừ" |
| **precedent** | câu hỏi + state tương tự | Án lệ: trích đoạn state + phán quyết + lý do, dùng làm few-shot |
| **fact** | workspace | Hiểu biết về một repo kèm hash file bằng chứng; tự hết hiệu lực khi file đổi |

- **Jury cấp thấp không bao giờ tự ghi memory** — chỉ judge và con người, để model thấp không tự củng cố lỗi của mình.
- Memory được tra và chèn vào prompt *trước khi* spawn agent; không cho agent tự tra bằng tool (mỗi lượt tool tốn thêm vài giây).
- `hungjury learn` cho model cao chấm lại ca treo và một mẫu quyết định cũ **ngoài luồng**, rồi gộp án lệ thành số ít ruling để prompt luôn ngắn.
- Phán quyết của judge và của người là ground truth ⇒ suy ra được độ chính xác từng juror ⇒ trọng số phiếu.
- Nếu mỗi lần bạn hỏi một câu hỏi ad-hoc khác nhau thì memory gần như vô dụng (chỉ còn `fact` giúp được). Lệnh `hungjury eval` tồn tại để đo điều này trên dữ liệu của chính bạn.

### 4. Memory: lưu local, chia sẻ bằng file

- Một file SQLite (FTS5) trong thư mục dữ liệu người dùng. Không có server, không đồng bộ qua mạng.
- Id của entry là hash nội dung, dữ liệu append-only ⇒ **merge là phép hợp tập**, import nhiều lần không trùng.
- `export` ra bundle JSONL (diff được trong git). **Mặc định chỉ xuất ruling và fact**; án lệ chứa trích đoạn state nên phải thêm `--include-cases`.
- `import`/`merge`: entry nhập về có mức tin cậy thấp hơn entry cục bộ; án lệ mâu thuẫn bị đánh dấu `contested` và chờ judge phân xử, không ghi đè âm thầm.

### 5. Gộp câu hỏi

Dồn mọi câu hỏi vào **một prompt** cho mỗi juror. Chi phí khởi động agent lớn, nên hỏi thừa vài câu mang tính suy đoán (speculative fan-out) rẻ hơn nhiều so với gọi thêm lần nữa.

### 6. An toàn và vận hành

- `state` là dữ liệu không tin cậy (prompt injection). State dạng text: agent **không có tool nào**. State dạng workspace: **chỉ tool đọc**. Không bao giờ bypass permission.
- Output bị ép kiểu (enum / số nguyên / boolean) nên injection tối đa chỉ lật được quyết định, không rò được dữ liệu. Lưu ý: sandbox `read-only` của codex vẫn đọc được toàn bộ filesystem.
- Memory làm giảm tính độc lập giữa các juror (cùng đọc một án lệ ⇒ dễ đồng thuận giả). Response luôn ghi entry nào đã được dùng; có tuỳ chọn giữ một juror "mù" không xem memory để đối chứng.
- Timeout cho mỗi juror; juror quá hạn bị loại khỏi phiếu và được ghi lại trong `usage`.
- Cache theo hash của `state` + `questions` + cấu hình jury + phiên bản memory liên quan.
- Hạn mức số lần gọi mỗi ngày + log `calls.jsonl`.

### 7. Pattern sử dụng

- **Speculative fan-out:** hỏi nhiều câu trong một call, code quyết định câu nào liên quan.
- **Confidence-gated routing:** exit code `2` / `hung` ⇒ chuyển cho người hoặc quy trình kỹ hơn.
- **Composite scoring:** gộp nhiều `score`/`noul` thành một điểm chung.
- **Intent routing:** `choice` phân loại ý định rồi chuyển tới handler phù hợp.
- **Dạy một lần, dùng mãi:** `hungjury feedback` sửa một quyết định sai ⇒ thành án lệ có độ tin cậy cao nhất.

## Quyết định còn mở

- [ ] Memory có thật sự kéo được model thấp lên gần model cao không — trả lời bằng thí nghiệm `eval` ở Phase 2 (mốc go/no-go, xem PLAN).
- [ ] Model juror mặc định cho devin (`swe-2-medium` miễn phí và khác họ model ⇒ phiếu độc lập hơn, hay `gpt-5-6-terra-low`) — chốt sau khi đo latency.
- [ ] Cấu hình tool chỉ-đọc cho `claude -p` khi không bypass permission — cần kiểm ở Phase 0.
- [ ] Tách lớp backend dùng chung với agentwiki thành crate riêng (sau khi API ổn định).

Các mục mở trước đây (ngôn ngữ, flag headless, công thức confidence, trọng số juror) đã chốt — xem bảng ở [docs/PLAN.md §9](docs/PLAN.md).
