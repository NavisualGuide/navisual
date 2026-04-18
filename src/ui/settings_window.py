"""Settings window for AI Navigator (v0.3.1).

Modal dialog with three tabs: Provider, Capture, Overlay.
Reads current config on open; writes .env atomically on Apply,
clears the config cache, and emits applied(new_config) so the
Application can push live changes to engine components.
"""

import os
from pathlib import Path
from typing import Optional

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QColor, QFont
from PySide6.QtWidgets import (
    QCheckBox,
    QColorDialog,
    QComboBox,
    QDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QSlider,
    QSpinBox,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from src.config import Config, get_config

# ── Colour palette (matches panel_window.py) ──────────────────────────────────
_BG       = "#1e1e1e"
_BG_MID   = "#2a2a2a"
_BG_TITLE = "#252525"
_TEXT     = "#d4d4d4"
_TEXT_DIM = "#888888"
_ORANGE   = "#FF6B35"
_ORANGE_D = "#E55A2B"
_BORDER   = "#444444"


# ── Helpers ───────────────────────────────────────────────────────────────────

def _label(text: str, dim: bool = False) -> QLabel:
    lbl = QLabel(text)
    lbl.setFont(QFont("Segoe UI", 9))
    lbl.setStyleSheet(f"color:{'#888888' if dim else _TEXT}; background:transparent;")
    return lbl


def _section(title: str) -> QLabel:
    lbl = QLabel(title)
    lbl.setFont(QFont("Segoe UI", 8, QFont.Weight.Bold))
    lbl.setStyleSheet(
        f"color:{_ORANGE}; background:transparent; "
        "border-bottom:1px solid #444; padding-bottom:2px; margin-top:6px;"
    )
    return lbl


def _field_style(enabled: bool = True) -> str:
    bg = _BG if enabled else "#222222"
    color = _TEXT if enabled else _TEXT_DIM
    return (
        f"QLineEdit, QComboBox, QSpinBox {{ background:{bg}; color:{color}; "
        f"border:1px solid {_BORDER}; border-radius:3px; padding:3px 5px; }}"
        f"QLineEdit:focus, QComboBox:focus, QSpinBox:focus {{ border-color:{_ORANGE}; }}"
        f"QComboBox::drop-down {{ border:none; }}"
        f"QComboBox QAbstractItemView {{ background:{_BG_MID}; color:{_TEXT}; "
        f"  selection-background-color:{_ORANGE}; }}"
    )


def _slider_row(slider: QSlider, value_lbl: QLabel) -> QWidget:
    row = QWidget()
    row.setStyleSheet("background:transparent;")
    h = QHBoxLayout(row)
    h.setContentsMargins(0, 0, 0, 0)
    h.setSpacing(8)
    h.addWidget(slider, stretch=1)
    h.addWidget(value_lbl)
    return row


def _make_slider(lo: int, hi: int, value: int, step: int = 1) -> tuple[QSlider, QLabel]:
    s = QSlider(Qt.Orientation.Horizontal)
    s.setRange(lo, hi)
    s.setValue(value)
    s.setSingleStep(step)
    s.setStyleSheet(
        f"QSlider::groove:horizontal {{ height:4px; background:#444; border-radius:2px; }}"
        f"QSlider::handle:horizontal {{ background:{_ORANGE}; width:12px; height:12px; "
        f"  margin:-4px 0; border-radius:6px; }}"
        f"QSlider::sub-page:horizontal {{ background:{_ORANGE}; border-radius:2px; }}"
    )
    lbl = QLabel(str(value))
    lbl.setFont(QFont("Segoe UI", 9))
    lbl.setFixedWidth(36)
    lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
    lbl.setStyleSheet(f"color:{_TEXT}; background:transparent;")
    s.valueChanged.connect(lambda v: lbl.setText(str(v)))
    return s, lbl


def _read_env_raw() -> list[str]:
    """Read .env lines, returning [] if file doesn't exist."""
    p = Path(".env")
    if p.exists():
        return p.read_text(encoding="utf-8").splitlines()
    return []


def write_env(updates: dict[str, str]) -> None:
    """Atomically update .env with the given key=value pairs.

    Preserves all existing lines (comments, blank lines, untouched keys).
    Existing uncommented keys are updated in-place.
    New keys are appended at the end.
    """
    lines = _read_env_raw()
    written: set[str] = set()
    new_lines: list[str] = []

    for line in lines:
        stripped = line.strip()
        # Blank or comment lines: keep as-is
        if not stripped or stripped.startswith("#"):
            new_lines.append(line)
            continue
        # Active key=value line
        if "=" in stripped:
            key = stripped.split("=", 1)[0].strip()
            if key in updates:
                new_lines.append(f"{key}={updates[key]}")
                written.add(key)
                continue
        new_lines.append(line)

    # Append any keys not already present
    for key, val in updates.items():
        if key not in written:
            new_lines.append(f"{key}={val}")

    tmp = Path(".env.tmp")
    tmp.write_text("\n".join(new_lines) + "\n", encoding="utf-8")
    tmp.replace(Path(".env"))


# ── Main dialog ───────────────────────────────────────────────────────────────

class SettingsWindow(QDialog):
    """Modal settings dialog."""

    # Emitted after Apply — carries the freshly loaded Config.
    applied = Signal(object)

    def __init__(self, parent: Optional[QWidget] = None) -> None:
        super().__init__(parent)
        self.setWindowTitle("AI Navigator — Settings")
        self.setFixedSize(500, 580)
        self.setModal(True)
        self.setStyleSheet(
            f"QDialog {{ background:{_BG}; color:{_TEXT}; }}"
            f"QTabWidget::pane {{ border:1px solid {_BORDER}; background:{_BG}; }}"
            f"QTabBar::tab {{ background:{_BG_TITLE}; color:{_TEXT_DIM}; "
            f"  padding:6px 14px; border:1px solid {_BORDER}; "
            f"  border-bottom:none; margin-right:2px; border-radius:3px 3px 0 0; }}"
            f"QTabBar::tab:selected {{ background:{_BG}; color:{_TEXT}; "
            f"  border-bottom:1px solid {_BG}; }}"
            f"QGroupBox {{ color:{_TEXT_DIM}; border:1px solid {_BORDER}; "
            f"  border-radius:4px; margin-top:8px; padding-top:8px; }}"
            f"QGroupBox::title {{ subcontrol-origin:margin; left:8px; "
            f"  color:{_TEXT_DIM}; padding:0 4px; }}"
            f"QCheckBox {{ color:{_TEXT}; spacing:6px; }}"
            f"QCheckBox::indicator {{ width:14px; height:14px; "
            f"  border:1px solid {_BORDER}; border-radius:2px; background:{_BG_MID}; }}"
            f"QCheckBox::indicator:checked {{ background:{_ORANGE}; border-color:{_ORANGE}; }}"
            f"QScrollArea {{ border:none; background:{_BG}; }}"
            f"QScrollBar:vertical {{ width:6px; background:{_BG_MID}; border:none; }}"
            f"QScrollBar::handle:vertical {{ background:#555; border-radius:3px; min-height:20px; }}"
            f"QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height:0; }}"
        )

        self._color = QColor(_ORANGE)   # tracks overlay color pick
        self._needs_restart = False

        self._build_ui()
        self._load_values(get_config())

    # ── Build ──────────────────────────────────────────────────────────────────

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(12, 12, 12, 12)
        root.setSpacing(8)

        self._tabs = QTabWidget()
        self._tabs.addTab(self._build_provider_tab(), "Provider")
        self._tabs.addTab(self._build_capture_tab(),  "Capture")
        self._tabs.addTab(self._build_overlay_tab(),  "Overlay")
        self._tabs.addTab(self._build_hotkeys_tab(),  "Hotkeys")
        root.addWidget(self._tabs, stretch=1)

        # Restart notice
        self._restart_lbl = QLabel("🔄  Some settings require a restart to take effect.")
        self._restart_lbl.setFont(QFont("Segoe UI", 8))
        self._restart_lbl.setStyleSheet(f"color:#F57F17; background:transparent; padding:2px 0;")
        self._restart_lbl.setVisible(False)
        root.addWidget(self._restart_lbl)

        # Restore Defaults / Cancel / Apply
        btn_row = QHBoxLayout()
        restore_btn = QPushButton("Restore All to Defaults")
        restore_btn.setFixedHeight(28)
        restore_btn.setToolTip("Reset all settings to factory defaults (API keys are kept).\nClick Apply to save.")
        restore_btn.setStyleSheet(
            f"QPushButton {{ background:{_BG_MID}; color:{_TEXT_DIM}; border:1px solid {_BORDER}; "
            f"  border-radius:4px; padding:0 10px; }}"
            f"QPushButton:hover {{ background:#333; color:{_TEXT}; }}"
        )
        restore_btn.clicked.connect(self._restore_defaults)
        btn_row.addWidget(restore_btn)
        btn_row.addStretch()
        cancel = QPushButton("Cancel")
        cancel.setFixedSize(80, 28)
        cancel.setStyleSheet(
            f"QPushButton {{ background:{_BG_MID}; color:{_TEXT}; border:1px solid {_BORDER}; "
            f"  border-radius:4px; }}"
            f"QPushButton:hover {{ background:#333; }}"
        )
        cancel.clicked.connect(self.reject)
        self._apply_btn = QPushButton("Apply")
        self._apply_btn.setFixedSize(80, 28)
        self._apply_btn.setStyleSheet(
            f"QPushButton {{ background:{_ORANGE}; color:white; border:none; border-radius:4px; }}"
            f"QPushButton:hover {{ background:{_ORANGE_D}; }}"
        )
        self._apply_btn.clicked.connect(self._on_apply)
        btn_row.addWidget(cancel)
        btn_row.addSpacing(8)
        btn_row.addWidget(self._apply_btn)
        root.addLayout(btn_row)

    def _scrollable(self, inner: QWidget) -> QScrollArea:
        sa = QScrollArea()
        sa.setWidgetResizable(True)
        sa.setWidget(inner)
        sa.setFrameShape(QScrollArea.Shape.NoFrame)
        return sa

    # ── Provider tab ──────────────────────────────────────────────────────────

    def _build_provider_tab(self) -> QWidget:
        inner = QWidget()
        inner.setStyleSheet(f"background:{_BG};")
        form = QFormLayout(inner)
        form.setContentsMargins(12, 10, 12, 10)
        form.setSpacing(6)
        form.setLabelAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)

        # Provider selector
        form.addRow(_section("AI Provider"))
        self._provider = QComboBox()
        self._provider.addItems(["gemini", "anthropic", "ollama", "openai"])
        self._provider.setStyleSheet(_field_style())
        self._provider.currentTextChanged.connect(self._on_provider_changed)
        form.addRow(_label("Provider"), self._provider)

        # ── Gemini ────────────────────────────────────────────────────────────
        form.addRow(_section("Gemini"))
        self._gemini_key = self._make_password_field()
        self._gemini_model = QComboBox()
        self._gemini_model.addItems([
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-2.5-pro",
        ])
        self._gemini_model.setEditable(True)
        self._gemini_fast = QComboBox()
        self._gemini_fast.addItems([
            "gemini-2.5-flash-lite",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
        ])
        self._gemini_fast.setEditable(True)
        for w in (self._gemini_key, self._gemini_model, self._gemini_fast):
            w.setStyleSheet(_field_style())
        form.addRow(_label("API Key"), self._gemini_key)
        form.addRow(_label("Model"), self._gemini_model)
        form.addRow(_label("Fast Model"), self._gemini_fast)
        self._gemini_widgets = [self._gemini_key, self._gemini_model, self._gemini_fast]

        # ── Anthropic ─────────────────────────────────────────────────────────
        form.addRow(_section("Anthropic"))
        self._anthropic_key = self._make_password_field()
        self._anthropic_model = QComboBox()
        self._anthropic_model.addItems([
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-6",
        ])
        self._anthropic_model.setEditable(True)
        self._anthropic_fast = QComboBox()
        self._anthropic_fast.addItems([
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-6",
        ])
        self._anthropic_fast.setEditable(True)
        for w in (self._anthropic_key, self._anthropic_model, self._anthropic_fast):
            w.setStyleSheet(_field_style())
        form.addRow(_label("API Key"), self._anthropic_key)
        form.addRow(_label("Model"), self._anthropic_model)
        form.addRow(_label("Fast Model"), self._anthropic_fast)
        self._anthropic_widgets = [self._anthropic_key, self._anthropic_model, self._anthropic_fast]

        # ── Ollama ────────────────────────────────────────────────────────────
        form.addRow(_section("Ollama  (local, no API key)"))
        self._ollama_url = QLineEdit()
        self._ollama_url.setPlaceholderText("http://localhost:11434")
        self._ollama_model = QLineEdit()
        self._ollama_model.setPlaceholderText("llama3.2-vision")
        for w in (self._ollama_url, self._ollama_model):
            w.setStyleSheet(_field_style())
        form.addRow(_label("Server URL"), self._ollama_url)
        form.addRow(_label("Model"), self._ollama_model)
        self._ollama_widgets = [self._ollama_url, self._ollama_model]

        # ── OpenAI ────────────────────────────────────────────────────────────
        form.addRow(_section("OpenAI  (🔄 restart required)"))
        self._openai_key = self._make_password_field()
        self._openai_key.setPlaceholderText("sk-…  (leave blank to keep current)")
        self._openai_model = QComboBox()
        self._openai_model.addItems(["gpt-4o", "gpt-4o-mini", "gpt-4-turbo"])
        self._openai_model.setEditable(True)
        stub_note = _label("OpenAI support is in preview — basic guidance only.", dim=True)
        for w in (self._openai_key, self._openai_model):
            w.setStyleSheet(_field_style())
        form.addRow(_label("API Key"), self._openai_key)
        form.addRow(_label("Model"), self._openai_model)
        form.addRow("", stub_note)
        self._openai_widgets = [self._openai_key, self._openai_model]

        # ── Shared ────────────────────────────────────────────────────────────
        form.addRow(_section("Shared"))
        self._api_timeout = QSpinBox()
        self._api_timeout.setRange(5, 120)
        self._api_timeout.setSuffix(" s")
        self._api_retries = QSpinBox()
        self._api_retries.setRange(0, 5)
        for w in (self._api_timeout, self._api_retries):
            w.setStyleSheet(_field_style())
        form.addRow(_label("API Timeout"), self._api_timeout)
        form.addRow(_label("Max Retries"), self._api_retries)

        return self._scrollable(inner)

    def _make_password_field(self) -> QLineEdit:
        f = QLineEdit()
        f.setEchoMode(QLineEdit.EchoMode.Password)
        f.setPlaceholderText("sk-…  (leave blank to keep current)")
        return f

    def _on_provider_changed(self, provider: str) -> None:
        all_groups = {
            "gemini":    self._gemini_widgets,
            "anthropic": self._anthropic_widgets,
            "ollama":    self._ollama_widgets,
            "openai":    self._openai_widgets,
        }
        for name, widgets in all_groups.items():
            enabled = (name == provider)
            for w in widgets:
                w.setEnabled(enabled)
                w.setStyleSheet(_field_style(enabled))

    # ── Capture tab ───────────────────────────────────────────────────────────

    def _build_capture_tab(self) -> QWidget:
        inner = QWidget()
        inner.setStyleSheet(f"background:{_BG};")
        form = QFormLayout(inner)
        form.setContentsMargins(12, 10, 12, 10)
        form.setSpacing(8)
        form.setLabelAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)

        # ── Auto-continue ─────────────────────────────────────────────────────
        form.addRow(_section("Auto-Continue"))
        self._auto_advance = QCheckBox(
            "Auto-continue to next step on page navigation  ✅"
        )
        self._auto_advance.setToolTip(
            "When on, checkpoint steps complete automatically when a large screen change is\n"
            "detected (e.g. page navigation). When off (default), press → Next to advance."
        )
        form.addRow("", self._auto_advance)

        note = _label(
            "Off by default — the AI only re-queries when you ask.\n"
            "Enable for continuous guided walkthroughs.",
            dim=True,
        )
        note.setWordWrap(True)
        form.addRow("", note)

        self._adv_thresh_slider, self._adv_thresh_lbl = _make_slider(10, 90, 30)
        self._adv_thresh_lbl.setFixedWidth(30)
        thresh_row = _slider_row(self._adv_thresh_slider, self._adv_thresh_lbl)
        sens_lbl = _label("Trigger threshold")
        sens_lbl.setToolTip(
            "How much of the screen must change before auto-continue fires.\n"
            "Low (10–20): fires on small changes like a dialog box appearing.\n"
            "Medium (30): default — fires on full page loads / navigation.\n"
            "High (60–90): only fires on near-complete screen replacements.\n"
            "Has no effect when Auto-continue is off."
        )
        form.addRow(sens_lbl, thresh_row)
        thresh_note = _label("Low = more sensitive · High = less sensitive (default 30 = page navigation)", dim=True)
        form.addRow("", thresh_note)

        self._auto_advance.toggled.connect(self._adv_thresh_slider.setEnabled)
        self._auto_advance.toggled.connect(lambda on: self._adv_thresh_slider.setStyleSheet(
            f"QSlider::groove:horizontal {{ height:4px; background:{'#444' if on else '#333'}; border-radius:2px; }}"
            f"QSlider::handle:horizontal {{ background:{'#FF6B35' if on else '#555'}; width:12px; height:12px; "
            f"  margin:-4px 0; border-radius:6px; }}"
            f"QSlider::sub-page:horizontal {{ background:{'#FF6B35' if on else '#555'}; border-radius:2px; }}"
        ))

        return self._scrollable(inner)

    # ── Overlay tab ───────────────────────────────────────────────────────────

    def _build_overlay_tab(self) -> QWidget:
        inner = QWidget()
        inner.setStyleSheet(f"background:{_BG};")
        form = QFormLayout(inner)
        form.setContentsMargins(12, 10, 12, 10)
        form.setSpacing(8)
        form.setLabelAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)

        form.addRow(_section("Arrow & Highlight  (✅ live)"))

        # Color picker button
        self._color_btn = QPushButton()
        self._color_btn.setFixedSize(64, 24)
        self._color_btn.setToolTip("Click to choose overlay color")
        self._color_btn.clicked.connect(self._pick_color)
        self._update_color_btn()
        form.addRow(_label("Color"), self._color_btn)

        self._thickness_slider, self._thickness_lbl = _make_slider(1, 10, 4)
        form.addRow(_label("Thickness"), _slider_row(self._thickness_slider, self._thickness_lbl))

        form.addRow(_section("Subtitle  (✅ live)"))

        self._font_slider, self._font_lbl = _make_slider(10, 36, 18)
        form.addRow(_label("Font Size"), _slider_row(self._font_slider, self._font_lbl))

        self._opacity_slider, self._opacity_lbl = _make_slider(0, 255, 180)
        form.addRow(_label("BG Opacity"), _slider_row(self._opacity_slider, self._opacity_lbl))

        self._duration_spin = QSpinBox()
        self._duration_spin.setRange(0, 30)
        self._duration_spin.setSpecialValueText("auto")
        self._duration_spin.setSuffix(" s")
        self._duration_spin.setToolTip(
            "How long the subtitle stays on screen.\n"
            "'auto' (0) = subtitle persists until the next instruction arrives\n"
            "         or you press Esc to dismiss it manually.\n"
            "1–30 s   = subtitle hides itself after the chosen number of seconds."
        )
        self._duration_spin.setStyleSheet(_field_style())
        self._duration_spin.setFixedWidth(80)
        form.addRow(_label("Duration"), self._duration_spin)

        return self._scrollable(inner)

    # ── Hotkeys tab ───────────────────────────────────────────────────────────

    def _build_hotkeys_tab(self) -> QWidget:
        inner = QWidget()
        inner.setStyleSheet(f"background:{_BG};")
        form = QFormLayout(inner)
        form.setContentsMargins(12, 10, 12, 10)
        form.setSpacing(6)
        form.setLabelAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)

        form.addRow(_section("Global Hotkeys  (🔄 restart required)"))

        note = _label(
            "Format: alt+e · ctrl+shift+x · alt+`\n"
            "Modifiers: alt  ctrl  shift   (combine with +)\n"
            "Keys: any letter, digit, or:  space  `  f1–f5  f12",
            dim=True,
        )
        note.setWordWrap(True)
        form.addRow(note)

        def _hotkey_field(placeholder: str = "") -> QLineEdit:
            f = QLineEdit()
            f.setPlaceholderText(placeholder)
            f.setStyleSheet(_field_style())
            f.setFixedWidth(160)
            f.setFont(QFont("Consolas", 9))
            return f

        self._next_step_hotkey    = _hotkey_field("alt+`")
        self._correction_hotkey_w = _hotkey_field("alt+e")
        self._pause_hotkey_w      = _hotkey_field("alt+s")
        self._floating_hotkey_w   = _hotkey_field("alt+q")
        self._talk_hotkey_w       = _hotkey_field("alt+a")
        self._reread_hotkey_w     = _hotkey_field("alt+r")

        form.addRow(_label("Next step"),     self._next_step_hotkey)
        form.addRow(_label("Re-analyze"),    self._correction_hotkey_w)
        form.addRow(_label("Pause/resume"),  self._pause_hotkey_w)
        form.addRow(_label("Toggle panel"),  self._floating_hotkey_w)
        form.addRow(_label("Talk (voice)"),  self._talk_hotkey_w)
        form.addRow(_label("Re-read"),       self._reread_hotkey_w)

        return self._scrollable(inner)

    def _pick_color(self) -> None:
        c = QColorDialog.getColor(self._color, self, "Overlay Color")
        if c.isValid():
            self._color = c
            self._update_color_btn()

    def _update_color_btn(self) -> None:
        hex_color = self._color.name()
        self._color_btn.setText(hex_color)
        self._color_btn.setStyleSheet(
            f"QPushButton {{ background:{hex_color}; color:white; border:none; "
            f"  border-radius:3px; font-size:9px; font-family:'Segoe UI'; }}"
        )

    # ── Load / Save ───────────────────────────────────────────────────────────

    def _load_values(self, config: Config) -> None:
        """Populate all fields from the current config."""
        # Provider
        idx = self._provider.findText(config.api_provider)
        self._provider.setCurrentIndex(max(0, idx))

        # Gemini
        self._gemini_key.setText("")          # never pre-fill API keys
        self._gemini_key.setPlaceholderText(
            "●●●●●●●●  (saved)" if config.gemini_api_key else "AIzaSy…"
        )
        self._set_combo(self._gemini_model, config.gemini_model)
        self._set_combo(self._gemini_fast,  config.gemini_fast_model)

        # Anthropic
        self._anthropic_key.setText("")
        self._anthropic_key.setPlaceholderText(
            "●●●●●●●●  (saved)" if config.anthropic_api_key else "sk-ant-…"
        )
        self._set_combo(self._anthropic_model, config.anthropic_model)
        self._set_combo(self._anthropic_fast,  config.anthropic_fast_model)

        # Ollama
        self._ollama_url.setText(config.ollama_base_url)
        self._ollama_model.setText(config.ollama_model)

        # OpenAI
        self._openai_key.setText("")
        self._openai_key.setPlaceholderText(
            "●●●●●●●●  (saved)" if config.openai_api_key else "sk-…"
        )
        self._set_combo(self._openai_model, getattr(config, "openai_model", "gpt-4o"))

        # Shared
        self._api_timeout.setValue(config.api_timeout_sec)
        self._api_retries.setValue(config.api_max_retries)

        # Trigger provider greying
        self._on_provider_changed(config.api_provider)

        # Capture
        self._auto_advance.setChecked(config.checkpoint_auto_advance)
        thresh_int = int(config.checkpoint_auto_advance_threshold * 100)
        self._adv_thresh_slider.setValue(thresh_int)
        self._adv_thresh_slider.setEnabled(config.checkpoint_auto_advance)

        # Overlay
        self._color = QColor(config.overlay_color)
        self._update_color_btn()
        self._thickness_slider.setValue(config.overlay_thickness)
        self._font_slider.setValue(config.subtitle_font_size)
        self._opacity_slider.setValue(config.subtitle_bg_opacity)
        self._duration_spin.setValue(getattr(config, "subtitle_duration_sec", 0))

        # Hotkeys
        self._next_step_hotkey.setText(config.next_step_hotkey)
        self._correction_hotkey_w.setText(config.correction_hotkey)
        self._pause_hotkey_w.setText(config.pause_hotkey)
        self._floating_hotkey_w.setText(config.floating_window_hotkey)
        self._talk_hotkey_w.setText(getattr(config, "talk_hotkey", "alt+a"))
        self._reread_hotkey_w.setText(getattr(config, "reread_hotkey", "alt+r"))

    def _set_combo(self, combo: QComboBox, value: str) -> None:
        idx = combo.findText(value)
        if idx >= 0:
            combo.setCurrentIndex(idx)
        else:
            combo.setCurrentText(value)

    # ── Restore Defaults ──────────────────────────────────────────────────────

    def _restore_defaults(self) -> None:
        """Reset all UI fields to factory defaults. API keys are left unchanged.
        Changes are not saved until the user clicks Apply."""
        self._set_combo(self._provider, "anthropic")

        # Models (not keys)
        self._set_combo(self._anthropic_model, "claude-sonnet-4-6")
        self._set_combo(self._anthropic_fast,  "claude-haiku-4-5-20251001")
        self._set_combo(self._gemini_model,    "gemini-2.5-flash")
        self._set_combo(self._gemini_fast,     "gemini-2.5-flash-lite")
        self._ollama_url.setText("http://localhost:11434")
        self._ollama_model.setText("llama3.2-vision")
        self._set_combo(self._openai_model,    "gpt-4o")

        # Shared
        self._api_timeout.setValue(30)
        self._api_retries.setValue(3)

        # Capture
        self._auto_advance.setChecked(False)
        self._adv_thresh_slider.setValue(30)

        # Overlay
        self._color = QColor("#FF6B35")
        self._update_color_btn()
        self._thickness_slider.setValue(4)
        self._font_slider.setValue(18)
        self._opacity_slider.setValue(180)
        self._duration_spin.setValue(0)

        # Hotkeys
        self._next_step_hotkey.setText("alt+`")
        self._correction_hotkey_w.setText("alt+e")
        self._pause_hotkey_w.setText("alt+s")
        self._floating_hotkey_w.setText("alt+q")
        self._talk_hotkey_w.setText("alt+a")
        self._reread_hotkey_w.setText("alt+r")

    # ── Apply ─────────────────────────────────────────────────────────────────

    def _on_apply(self) -> None:
        updates: dict[str, str] = {}

        # Provider
        provider = self._provider.currentText()
        updates["API_PROVIDER"] = provider

        # Gemini
        if self._gemini_key.text().strip():
            updates["GEMINI_API_KEY"] = self._gemini_key.text().strip()
        updates["GEMINI_MODEL"]      = self._gemini_model.currentText()
        updates["GEMINI_FAST_MODEL"] = self._gemini_fast.currentText()

        # Anthropic
        if self._anthropic_key.text().strip():
            updates["ANTHROPIC_API_KEY"] = self._anthropic_key.text().strip()
        updates["ANTHROPIC_MODEL"]      = self._anthropic_model.currentText()
        updates["ANTHROPIC_FAST_MODEL"] = self._anthropic_fast.currentText()

        # Ollama
        updates["OLLAMA_BASE_URL"] = self._ollama_url.text().strip() or "http://localhost:11434"
        updates["OLLAMA_MODEL"]    = self._ollama_model.text().strip() or "llama3.2-vision"

        # OpenAI
        if self._openai_key.text().strip():
            updates["OPENAI_API_KEY"] = self._openai_key.text().strip()
        updates["OPENAI_MODEL"] = self._openai_model.currentText() or "gpt-4o"

        # Shared
        updates["API_TIMEOUT_SEC"]  = str(self._api_timeout.value())
        updates["API_MAX_RETRIES"]  = str(self._api_retries.value())

        # Capture
        updates["CHECKPOINT_AUTO_ADVANCE"]          = str(self._auto_advance.isChecked()).lower()
        updates["CHECKPOINT_AUTO_ADVANCE_THRESHOLD"] = f"{self._adv_thresh_slider.value() / 100:.2f}"

        # Overlay
        updates["OVERLAY_COLOR"]         = self._color.name()
        updates["OVERLAY_THICKNESS"]     = str(self._thickness_slider.value())
        updates["SUBTITLE_FONT_SIZE"]    = str(self._font_slider.value())
        updates["SUBTITLE_BG_OPACITY"]   = str(self._opacity_slider.value())
        updates["SUBTITLE_DURATION_SEC"] = str(self._duration_spin.value())

        # Hotkeys
        if self._next_step_hotkey.text().strip():
            updates["NEXT_STEP_HOTKEY"]       = self._next_step_hotkey.text().strip()
        if self._correction_hotkey_w.text().strip():
            updates["CORRECTION_HOTKEY"]      = self._correction_hotkey_w.text().strip()
        if self._pause_hotkey_w.text().strip():
            updates["PAUSE_HOTKEY"]           = self._pause_hotkey_w.text().strip()
        if self._floating_hotkey_w.text().strip():
            updates["FLOATING_WINDOW_HOTKEY"] = self._floating_hotkey_w.text().strip()
        if self._talk_hotkey_w.text().strip():
            updates["TALK_HOTKEY"]            = self._talk_hotkey_w.text().strip()
        if self._reread_hotkey_w.text().strip():
            updates["REREAD_HOTKEY"]          = self._reread_hotkey_w.text().strip()

        # Write .env atomically and reload config
        write_env(updates)
        get_config.cache_clear()
        new_config = get_config()

        # Determine if any 🔄 (restart-required) settings changed
        restart_keys = {
            "API_PROVIDER", "GEMINI_API_KEY", "GEMINI_MODEL", "GEMINI_FAST_MODEL",
            "ANTHROPIC_API_KEY", "ANTHROPIC_MODEL", "ANTHROPIC_FAST_MODEL",
            "OLLAMA_BASE_URL", "OLLAMA_MODEL", "API_TIMEOUT_SEC", "API_MAX_RETRIES",
            "OPENAI_API_KEY", "OPENAI_MODEL",
            "NEXT_STEP_HOTKEY", "CORRECTION_HOTKEY", "PAUSE_HOTKEY",
            "FLOATING_WINDOW_HOTKEY", "TALK_HOTKEY", "REREAD_HOTKEY",
        }
        old = get_config()  # same instance since just reloaded
        self._restart_lbl.setVisible(any(k in updates for k in restart_keys))

        self._apply_btn.setText("Applied ✓")
        self._apply_btn.setEnabled(False)
        from PySide6.QtCore import QTimer
        QTimer.singleShot(1500, lambda: (
            self._apply_btn.setText("Apply"),
            self._apply_btn.setEnabled(True),
        ))

        self.applied.emit(new_config)
