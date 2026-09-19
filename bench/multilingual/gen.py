#!/usr/bin/env python3
"""Generate the `multilingual` eval set (~60 cases).

Use case: same support-ticket triage as `support`, but tickets arrive in
Vietnamese and mixed vi/en. Identical question ids (support.department,
support.frustration, support.is_urgent) and the same labeling policy so the
result is directly comparable to the English `support` domain.

  * charged wrongly / money owed / refund        -> billing
  * payment cannot complete due to a bug         -> technical
  * pricing/plan/quote questions                 -> sales
  * product defects with no money question       -> technical
  * frustration: 0 calm, 1 frustrated, 2 angry
  * is_urgent: explicit deadline/blocking/ASAP language (either language)
"""
import json, random

rng = random.Random(20260924)

Q = {
    "department": {
        "type": "choice",
        "id": "support.department",
        "instructions": "Which team should handle this ticket",
        "criteria": {
            "billing": "Payment, charge, refund or subscription money issues",
            "technical": "Bugs, crashes, errors or integration problems",
            "sales": "Pricing, plan or account questions before purchase",
        },
    },
    "frustration": {
        "type": "score",
        "id": "support.frustration",
        "instructions": "How frustrated the customer appears",
        "criteria": [
            "Calm, just stating facts",
            "Frustrated but civil",
            "Very angry, strong language",
        ],
    },
    "is_urgent": {
        "type": "noul",
        "id": "support.is_urgent",
        "instructions": "The message conveys urgency or time-sensitivity (deadline, blocking, ASAP)",
    },
}

cases = []
def add(state, dept, frust, urg):
    cases.append({"state": state, "questions": Q,
                  "expected": {"department": dept, "frustration": frust, "is_urgent": urg}})

CALM_VI = ["Chỉ báo để team biết.", "Không gấp đâu.", "Báo cho biết thôi.", ""]
FRUST_VI = ["Hơi bực rồi đó.", "Tình trạng này kéo dài lắm rồi.", "Không hài lòng chút nào."]
ANGRY_VI = ["Quá đáng lắm rồi!!", "Tôi cực kỳ THẤT VỌNG.", "Vô lý — sửa NGAY cho tôi."]
URG_VI = ["Đang block việc chốt sổ cuối tháng — cần gấp.", "Mai chúng tôi release, đây là blocker.",
          "Hệ thống production đang sập — khẩn cấp.", "Hạn pháp lý là thứ Sáu này."]
NOTURG_VI = ["Khi nào rảnh xử lý cũng được.", "Không có deadline cụ thể.", "Không gấp.", ""]

PLANS = ["Pro", "Team", "Business", "Starter"]
AMTS = ["450k", "1.2tr", "$49", "$99", "2.4tr", "$240"]

def pick_tone(fr):
    return [rng.choice(CALM_VI), rng.choice(FRUST_VI), rng.choice(ANGRY_VI)][fr]

# ---------- billing vi (16) ----------
billing_vi = [
    "Tôi bị trừ tiền 2 lần cho gói {plan} — {amt} ngày 3 và {amt} ngày 5. Vui lòng hoàn lại khoản trùng. {tone} {urg}",
    "Hoá đơn tháng này ghi {amt} trong khi gói {plan} của tôi phải rẻ hơn. Kiểm tra lại giúp. {tone} {urg}",
    "Tôi đã huỷ subscription tháng trước mà vẫn bị charge {amt}. Đề nghị refund. {tone} {urg}",
    "Có một khoản charge {amt} từ công ty các bạn trên thẻ của tôi mà tôi không nhận ra. Đó là phí gì? {tone} {urg}",
]
for i in range(16):
    t = rng.choice(billing_vi)
    fr = rng.choices([0, 1, 2], weights=[3, 4, 3])[0]
    urg = rng.random() < 0.4
    add(t.format(plan=rng.choice(PLANS), amt=rng.choice(AMTS), tone=pick_tone(fr),
                 urg=rng.choice(URG_VI) if urg else rng.choice(NOTURG_VI)),
        "billing", fr, urg)

# ---------- technical vi (16) ----------
tech_vi = [
    "Trang thanh toán bị crash mỗi lần tôi mở — từ sau bản update gần nhất. {tone} {urg}",
    "API của các bạn trả về HTTP 500 ở endpoint /v1/orders từ sáng nay. {tone} {urg}",
    "Đăng nhập SSO báo lỗi 'invalid_state' trên cả Chrome lẫn Firefox. {tone} {urg}",
    "File CSV export ra bị lỗi — cột ngày bị lệch một ngày từ dòng 500 trở đi. {tone} {urg}",
    "App mobile đơ ở màn hình splash khoảng 10 giây trên Android 15. {tone} {urg}",
]
for i in range(16):
    t = rng.choice(tech_vi)
    fr = rng.choices([0, 1, 2], weights=[4, 4, 2])[0]
    urg = rng.random() < 0.4
    add(t.format(tone=pick_tone(fr),
                 urg=rng.choice(URG_VI) if urg else rng.choice(NOTURG_VI)),
        "technical", fr, urg)

# ---------- sales vi (10) ----------
sales_vi = [
    "Gói {plan} có hỗ trợ SSO không? Đang so với gói Team trước khi mua. {tone} {urg}",
    "Cho mình xin báo giá 120 seats gói {plan}? Có giảm giá cho giáo dục không? {tone} {urg}",
    "Gói {plan} khác Enterprise ở chỗ retention dữ liệu như thế nào? {tone} {urg}",
    "Có phương án thanh toán theo tháng cho gói {plan} không hay chỉ theo năm? {tone} {urg}",
]
for i in range(10):
    t = rng.choice(sales_vi)
    fr = 0 if rng.random() < 0.8 else 1
    urg = rng.random() < 0.2
    add(t.format(plan=rng.choice(PLANS), tone=pick_tone(fr),
                 urg=rng.choice(URG_VI) if urg else rng.choice(NOTURG_VI)),
        "sales", fr, urg)

# ---------- mixed vi/en (18): half the message in each language ----------
mixed_t = [
    ("App của mình crash lúc checkout and it charged me {amt} twice. Please refund the duplicate — hoàn tiền gấp giúp mình. {tone} {urg}", "billing"),
    ("I can't update my credit card — trang billing settings cứ báo JavaScript error và không save được. {tone} {urg}", "technical"),
    ("Does the {plan} plan include SSO? Bọn mình đang compare với Team plan trước khi quyết định mua. {tone} {urg}", "sales"),
    ("My invoice for last month shows {amt} nhưng gói {plan} của tôi đáng lẽ rẻ hơn — can you correct the charge? {tone} {urg}", "billing"),
    ("Getting HTTP 500 từ REST API endpoint /v1/orders since this morning — mọi request đều fail. {tone} {urg}", "technical"),
]
for i in range(18):
    t, dept = rng.choice(mixed_t)
    fr = rng.choices([0, 1, 2], weights=[4, 4, 2])[0]
    urg = rng.random() < 0.4
    # tone may be vi or en for extra mixing
    tone = pick_tone(fr) if rng.random() < 0.7 else ["FYI.", "Pretty annoying.", "Unacceptable!!"][fr]
    u = rng.choice(URG_VI) if urg else rng.choice(NOTURG_VI)
    add(t.format(plan=rng.choice(PLANS), amt=rng.choice(AMTS), tone=tone, urg=u), dept, fr, urg)

rng.shuffle(cases)
with open("bench/multilingual/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["department"] for c in cases))
print("urgent:", sum(c["expected"]["is_urgent"] for c in cases),
      "| frustration:", Counter(c["expected"]["frustration"] for c in cases))
