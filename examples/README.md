# Examples — runnable spikes

Mỗi thư mục con là một project `.hungjury/` tự chứa: config, `policy.md`,
memory.db riêng — memory của các use case không bao giờ lẫn nhau.

## support-triage — phân luồng ticket hàng loạt

```bash
cd examples/support-triage
hungjury batch cases.jsonl --out results.jsonl   # 4 tickets, song song
hungjury memory decisions --last 10              # ai quyết, hung nào
```

`results.jsonl`: một dòng JSON per case — `case` (label echo từ input),
`answers`, `decided_by`, `hung`, `exit`. Ticket treo (exit 2) nằm trong
hàng chờ: duyệt bằng `hungjury feedback <id> --set key=val` hoặc
`hungjury learn --audit --recent 10`.

## pr-review — cổng review trên workspace

```bash
cd examples/pr-review
hungjury decide --questions @questions.json \
    --workspace ../../            # repo hungjury, hoặc repo bất kỳ
    --hint "Xem diff HEAD~3..HEAD, đánh giá mức độ cần review"
```

Juror mở file/diff tự khám phá (read-only). `escalate=sync`: PR khó →
judge quyết ngay trong luồng. Rulings lưu theo `ws:<repo>` scope.

## log-triage — phân loại lỗi CI

```bash
cd examples/log-triage
hungjury decide --state-file failure.log --questions @questions.json
# hoặc pipe trực tiếp từ CI (`--state-file -` = stdin):
tail -50 build.log | hungjury decide --state-file - --questions @questions.json
```

## Dọn sạch

```bash
rm -f examples/*/.hungjury/memory.db examples/*/.hungjury/calls.jsonl
rm -rf examples/*/.hungjury/cache examples/*/.hungjury/state.json
```
