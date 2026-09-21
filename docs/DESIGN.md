# Thiết kế

## 1. Adapter cho từng CLI

Ba backend: **claude**, **codex**, **devin**; model viết dạng `<backend>:<model>[@<effort>]`, ví dụ `codex:gpt-5.6-terra@low`. Lớp backend tái sử dụng từ project [agentwiki](https://github.com/tidusvn05/agentwiki).

- Chạy CLI như subprocess headless; loại bỏ mọi env API key để không vô tình chuyển sang billing API.
- Ép output theo JSON schema sinh từ `questions`: claude dùng `--json-schema`, codex dùng `--output-schema` (cả hai đã chạy thử); devin không có flag schema nên ép bằng prompt.
- Luôn validate lại. Sai schema → retry có giới hạn kèm phản hồi lỗi → hết lượt thì juror đó bị loại khỏi phiếu. Không bao giờ trả dữ liệu sai kiểu.

## 2. Xác suất đến từ bỏ phiếu, không phải tự khai

- **Không** để model tự khai xác suất — con số đó calibrate kém.
- Chạy nhiều juror và/hoặc nhiều sample; **tỉ lệ phiếu là phân phối xác suất**.
- **Mức bất đồng là confidence:** `choice` = chênh lệch giữa hai lựa chọn đầu; `score` = 1 − độ lệch chuẩn đã chuẩn hoá; `noul` = |2p − 1|. Một phiếu duy nhất thì `confidence = null`.
- Jury "treo" khi confidence dưới ngưỡng (mặc định 0.5; với 3 juror, 2–1 là treo).

## 3. Hai tầng model và trí nhớ án lệ

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

## 4. Memory: lưu local, chia sẻ bằng file

- Một file SQLite (FTS5) trong thư mục dữ liệu người dùng. Không có server, không đồng bộ qua mạng.
- Id của entry là hash nội dung, dữ liệu append-only ⇒ **merge là phép hợp tập**, import nhiều lần không trùng.
- `export` ra bundle JSONL (diff được trong git). **Mặc định chỉ xuất ruling và fact**; án lệ chứa trích đoạn state nên phải thêm `--include-cases`.
- `import`/`merge`: entry nhập về có mức tin cậy thấp hơn entry cục bộ; án lệ mâu thuẫn bị đánh dấu `contested` và chờ judge phân xử, không ghi đè âm thầm.

## 5. Gộp câu hỏi

Dồn mọi câu hỏi vào **một prompt** cho mỗi juror. Chi phí khởi động agent lớn, nên hỏi thừa vài câu mang tính suy đoán (speculative fan-out) rẻ hơn nhiều so với gọi thêm lần nữa.

Cùng nguyên tắc ở cấp batch: `batch --pack N` gom N case (cùng bộ
questions) vào **một call** mỗi juror — juror trả ballot per-item
`{"<item-id>": {answers}}`, còn vote/quorum/hung/escalation vẫn tính
từng item. Item id mang nonce mỗi pack để nội dung state không forge
được tag của item lân cận; item parse lỗi chỉ mất ballot của item đó.
Workspace state không pack được (không chia sẻ chung prompt).

## 6. An toàn và vận hành

- `state` là dữ liệu không tin cậy (prompt injection). State dạng text: agent **không có tool nào**. State dạng workspace: **chỉ tool đọc**. Không bao giờ bypass permission.
- Output bị ép kiểu (enum / số nguyên / boolean) nên injection tối đa chỉ lật được quyết định, không rò được dữ liệu. Lưu ý: sandbox `read-only` của codex vẫn đọc được toàn bộ filesystem.
- Memory làm giảm tính độc lập giữa các juror (cùng đọc một án lệ ⇒ dễ đồng thuận giả). Response luôn ghi entry nào đã được dùng; có tuỳ chọn giữ một juror "mù" không xem memory để đối chứng.
- Timeout cho mỗi juror; juror quá hạn bị loại khỏi phiếu và được ghi lại trong `usage`.
- Cache theo hash của `state` + `questions` + cấu hình jury + phiên bản memory liên quan (kèm pack size, nên kết quả packed/unpacked không lẫn).
- Hạn mức số lần gọi mỗi ngày + log `calls.jsonl`.

## 7. Pattern sử dụng

- **Speculative fan-out:** hỏi nhiều câu trong một call, code quyết định câu nào liên quan.
- **Confidence-gated routing:** exit code `2` / `hung` ⇒ chuyển cho người hoặc quy trình kỹ hơn.
- **Composite scoring:** gộp nhiều `score`/`noul` thành một điểm chung.
- **Intent routing:** `choice` phân loại ý định rồi chuyển tới handler phù hợp.
- **Dạy một lần, dùng mãi:** `hungjury feedback` sửa một quyết định sai ⇒ thành án lệ có độ tin cậy cao nhất.
