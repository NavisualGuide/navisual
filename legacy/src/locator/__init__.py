"""Element locator: Accessibility API (primary, < 5ms), OCR fallback, template matching.

Strategy priority:
1. OS Accessibility API (UIA on Windows) — instant, accurate for browsers
2. PaddleOCR — fallback when A11y tree unavailable or sparse
3. Template matching — future (v0.3) for icon-only elements
"""
