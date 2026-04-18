"""Consolidated floating panel for AI Navigator.

Replaces MainWindow + FloatingWindow with a single always-on-top widget that:
- Panel mode (default): full chat UI with latest instruction, history, input, action buttons
- Icon mode: 56×56 draggable rounded icon; collapses panel out of the way

Excluded from mss screen capture via WDA_EXCLUDEFROMCAPTURE so it never
appears in screenshots sent to the AI.
"""

import logging
from typing import Optional

from PySide6.QtCore import Qt, QTimer, Signal
from src.config import Config, get_config
from PySide6.QtGui import QFont, QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QApplication,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollBar,
    QSizePolicy,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)

# ── Colour palette ────────────────────────────────────────────────────────────
_ORANGE      = "#FF6B35"
_ORANGE_DARK = "#E55A2B"
_BG          = "#1e1e1e"
_BG_MID      = "#2a2a2a"
_BG_TITLE    = "#252525"
_BG_LATEST   = "#2a1800"   # dark orange tint for latest-instruction box
_TEXT        = "#d4d4d4"
_TEXT_DIM    = "#888888"
_GREEN       = "#2E7D32"
_GREEN_DARK  = "#1B5E20"
_RED         = "#C62828"
_RED_DARK    = "#B71C1C"
_AMBER       = "#F57F17"
_AMBER_DARK  = "#E65100"
_BLUE        = "#1565C0"
_BLUE_DARK   = "#0D47A1"
_DOT_READY   = "#4CAF50"
_DOT_BUSY    = "#F57F17"
_DOT_ERROR   = "#E53935"

_TITLE_H = 32   # title bar height in pixels — drag threshold


def _btn(text: str, bg: str, hover: str, tooltip: str = "") -> QPushButton:
    """Helper to create a styled action button."""
    b = QPushButton(text)
    b.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
    b.setStyleSheet(
        f"QPushButton {{ background:{bg}; color:white; border:none;"
        f"  border-radius:4px; padding:4px 8px; }}"
        f"QPushButton:hover {{ background:{hover}; }}"
        f"QPushButton:disabled {{ background:#555; color:#888; }}"
    )
    if tooltip:
        b.setToolTip(tooltip)
    return b


def _icon_btn(text: str) -> QPushButton:
    """Small frameless icon button for title bar."""
    b = QPushButton(text)
    b.setFixedSize(22, 22)
    b.setStyleSheet(
        "QPushButton { background:transparent; color:#888; border:none; font-size:13px; }"
        "QPushButton:hover { color:white; }"
    )
    return b


class ConsolidatedPanel(QWidget):
    """Single floating panel that replaces MainWindow + FloatingWindow."""

    # ── Signals ───────────────────────────────────────────────────────────────
    message_submitted    = Signal(str)
    correction_requested = Signal()
    pause_toggled        = Signal()
    next_step_requested  = Signal()
    mic_pressed          = Signal()
    new_session_requested    = Signal()
    save_session_requested   = Signal()
    settings_requested       = Signal()
    overlay_dismiss_requested = Signal()  # Esc — hides subtitle/overlay

    # ── Construction ──────────────────────────────────────────────────────────

    def __init__(self) -> None:
        super().__init__(
            None,
            Qt.WindowType.FramelessWindowHint
            | Qt.WindowType.WindowStaysOnTopHint
            | Qt.WindowType.Tool,
        )
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)

        self._in_panel_mode  = True
        self._dragging       = False
        self._drag_offset    = None
        self._drag_started   = False   # True once movement exceeds threshold
        self._panel_pos      = None

        # Streaming state
        self._streaming_text   = ""
        self._streaming_active = False

        # Thinking animation (shown while waiting for API, before streaming starts)
        self._thinking_dots = 0
        self._thinking_timer = QTimer(self)
        self._thinking_timer.setInterval(500)
        self._thinking_timer.timeout.connect(self._tick_thinking)

        self._build_ui()
        self._add_shortcuts()
        self._position_bottom_right()

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self) -> None:
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)

        self._icon_w  = self._build_icon_widget()
        self._panel_w = self._build_panel_widget()

        outer.addWidget(self._icon_w)
        outer.addWidget(self._panel_w)

        # Start in panel mode
        self._icon_w.hide()
        self.setFixedSize(360, 540)

    def _build_icon_widget(self) -> QWidget:
        """56×56 orange rounded-square icon."""
        w = QWidget()
        w.setFixedSize(56, 56)
        w.setStyleSheet(
            f"QWidget {{ background:{_ORANGE}; border-radius:12px; }}"
        )
        layout = QVBoxLayout(w)
        layout.setContentsMargins(4, 4, 4, 4)
        layout.setSpacing(0)

        lbl = QLabel("N")
        lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lbl.setFont(QFont("Segoe UI", 22, QFont.Weight.Bold))
        lbl.setStyleSheet("color:white; background:transparent;")
        layout.addWidget(lbl)

        self._icon_dot = QLabel("●")
        self._icon_dot.setFont(QFont("Segoe UI", 8))
        self._icon_dot.setStyleSheet(f"color:{_DOT_READY}; background:transparent;")
        self._icon_dot.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignBottom)
        layout.addWidget(self._icon_dot)

        return w

    def _build_panel_widget(self) -> QWidget:
        """Full 360×540 panel."""
        w = QWidget()
        w.setStyleSheet(
            f"QWidget#panel {{ background:{_BG}; border:1px solid #444; border-radius:8px; }}"
        )
        w.setObjectName("panel")

        layout = QVBoxLayout(w)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)

        layout.addWidget(self._build_title_bar())
        layout.addWidget(self._build_latest_box())
        layout.addWidget(self._build_chat_history(), stretch=1)
        layout.addWidget(self._build_input_row())
        layout.addWidget(self._build_action_row())
        layout.addWidget(self._build_shortcut_legend())

        return w

    def _build_title_bar(self) -> QWidget:
        bar = QWidget()
        bar.setFixedHeight(_TITLE_H)
        bar.setObjectName("titlebar")
        bar.setStyleSheet(
            f"QWidget#titlebar {{ background:{_BG_TITLE};"
            "  border-top-left-radius:8px; border-top-right-radius:8px;"
            "  border-bottom:1px solid #333; }}"
        )

        h = QHBoxLayout(bar)
        h.setContentsMargins(8, 0, 4, 0)
        h.setSpacing(4)

        compass = QLabel("🧭")
        compass.setFont(QFont("Segoe UI", 12))
        compass.setStyleSheet("background:transparent;")
        h.addWidget(compass)

        title = QLabel("AI Navigator")
        title.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
        title.setStyleSheet(f"color:{_ORANGE}; background:transparent;")
        h.addWidget(title)

        self._status_dot = QLabel("●")
        self._status_dot.setFont(QFont("Segoe UI", 10))
        self._status_dot.setStyleSheet(f"color:{_DOT_READY}; background:transparent;")
        h.addWidget(self._status_dot)

        self._token_lbl = QLabel("")
        self._token_lbl.setFont(QFont("Segoe UI", 8))
        self._token_lbl.setStyleSheet(f"color:{_TEXT_DIM}; background:transparent;")
        h.addWidget(self._token_lbl)

        h.addStretch()

        gear_btn = _icon_btn("⚙")
        gear_btn.setToolTip("Settings")
        gear_btn.clicked.connect(self.settings_requested.emit)
        h.addWidget(gear_btn)

        new_btn = _icon_btn("＋")
        new_btn.setToolTip("New session (Ctrl+N)")
        new_btn.clicked.connect(self.new_session_requested.emit)
        h.addWidget(new_btn)

        save_btn = _icon_btn("💾")
        save_btn.setToolTip("Save session (Ctrl+S)")
        save_btn.clicked.connect(self.save_session_requested.emit)
        h.addWidget(save_btn)

        collapse_btn = QPushButton("⊟")
        collapse_btn.setFixedSize(32, 22)
        collapse_btn.setToolTip("Collapse to icon (Ctrl+Shift+Space)")
        collapse_btn.setFont(QFont("Segoe UI", 12))
        collapse_btn.setStyleSheet(
            f"QPushButton {{ background:{_ORANGE}; color:white; border:none; border-radius:3px; }}"
            f"QPushButton:hover {{ background:{_ORANGE_DARK}; }}"
        )
        collapse_btn.clicked.connect(self._to_icon_mode)
        h.addWidget(collapse_btn)

        quit_btn = _icon_btn("×")
        quit_btn.setToolTip("Quit AI Navigator")
        quit_btn.setStyleSheet(
            "QPushButton { background:transparent; color:#888; border:none; font-size:14px; }"
            "QPushButton:hover { color:#E53935; }"
        )
        quit_btn.clicked.connect(QApplication.quit)
        h.addWidget(quit_btn)

        return bar

    def _build_latest_box(self) -> QWidget:
        """Prominent latest-instruction display."""
        box = QWidget()
        box.setStyleSheet(
            f"QWidget {{ background:{_BG_LATEST}; border-left:3px solid {_ORANGE}; }}"
        )
        v = QVBoxLayout(box)
        v.setContentsMargins(10, 5, 8, 5)
        v.setSpacing(2)

        hdr_row = QHBoxLayout()
        hdr_row.setContentsMargins(0, 0, 0, 0)
        hdr_row.setSpacing(0)

        hdr = QLabel("Latest instruction")
        hdr.setFont(QFont("Segoe UI", 7))
        hdr.setStyleSheet(f"color:{_TEXT_DIM}; background:transparent; border:none;")
        hdr_row.addWidget(hdr)

        hdr_row.addStretch()

        self._step_lbl = QLabel("")
        self._step_lbl.setFont(QFont("Segoe UI", 7))
        self._step_lbl.setStyleSheet(f"color:{_TEXT_DIM}; background:transparent; border:none;")
        hdr_row.addWidget(self._step_lbl)

        v.addLayout(hdr_row)

        self._latest_lbl = QLabel("Waiting for task…")
        self._latest_lbl.setWordWrap(True)
        self._latest_lbl.setFont(QFont("Segoe UI", 10))
        self._latest_lbl.setStyleSheet(f"color:{_ORANGE}; background:transparent; border:none;")
        self._latest_lbl.setMinimumHeight(42)
        self._latest_lbl.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Minimum)
        v.addWidget(self._latest_lbl)

        return box

    def _build_chat_history(self) -> QWidget:
        self._chat = QTextEdit()
        self._chat.setReadOnly(True)
        self._chat.setFont(QFont("Segoe UI", 9))
        self._chat.setStyleSheet(
            f"QTextEdit {{ background:{_BG}; color:{_TEXT}; border:none; padding:4px 8px; }}"
            "QScrollBar:vertical { width:6px; background:#2a2a2a; border:none; }"
            "QScrollBar::handle:vertical { background:#555; border-radius:3px; min-height:20px; }"
            "QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height:0; }"
        )
        return self._chat

    def _build_input_row(self) -> QWidget:
        row = QWidget()
        row.setStyleSheet(f"QWidget {{ background:{_BG_MID}; border-top:1px solid #333; }}")
        h = QHBoxLayout(row)
        h.setContentsMargins(6, 6, 6, 6)
        h.setSpacing(4)

        self._input = QLineEdit()
        self._input.setPlaceholderText("Type your task or follow-up…")
        self._input.setFont(QFont("Segoe UI", 10))
        self._input.setStyleSheet(
            f"QLineEdit {{ background:{_BG}; color:{_TEXT}; border:1px solid #444;"
            "  border-radius:4px; padding:4px 6px; }}"
            f"QLineEdit:focus {{ border-color:{_ORANGE}; }}"
        )
        self._input.returnPressed.connect(self._on_send)
        h.addWidget(self._input, stretch=1)

        self._send_btn = QPushButton("→")
        self._send_btn.setFixedSize(32, 28)
        self._send_btn.setFont(QFont("Segoe UI", 14, QFont.Weight.Bold))
        self._send_btn.setStyleSheet(
            f"QPushButton {{ background:{_ORANGE}; color:white; border:none; border-radius:4px; }}"
            f"QPushButton:hover {{ background:{_ORANGE_DARK}; }}"
            "QPushButton:disabled { background:#555; color:#888; }"
        )
        self._send_btn.clicked.connect(self._on_send)
        h.addWidget(self._send_btn)

        return row

    def _build_action_row(self) -> QWidget:
        row = QWidget()
        row.setStyleSheet(f"QWidget {{ background:{_BG_MID}; border:none; }}")
        h = QHBoxLayout(row)
        h.setContentsMargins(6, 0, 6, 6)
        h.setSpacing(4)

        cfg = get_config()
        self._next_btn  = _btn("→ Next",   _GREEN, _GREEN_DARK, f"Next step ({cfg.next_step_hotkey})")
        self._wrong_btn = _btn("✗ Wrong",  _RED,   _RED_DARK,   f"Re-analyze ({cfg.correction_hotkey})")
        self._pause_btn = _btn("⏸ Pause",  _AMBER, _AMBER_DARK, f"Pause capture ({cfg.pause_hotkey})")
        self._mic_btn   = _btn("🎤 Speak", _BLUE,  _BLUE_DARK,  f"Push to talk ({cfg.talk_hotkey})")

        self._next_btn.clicked.connect(self.next_step_requested.emit)
        self._wrong_btn.clicked.connect(self.correction_requested.emit)
        self._pause_btn.clicked.connect(self._on_pause)
        self._mic_btn.clicked.connect(self.mic_pressed.emit)
        self._mic_btn.setVisible(False)

        h.addWidget(self._next_btn)
        h.addWidget(self._wrong_btn)
        h.addWidget(self._pause_btn)
        h.addWidget(self._mic_btn)

        return row

    def _build_shortcut_legend(self) -> QWidget:
        cfg = get_config()
        self._legend_lbl = QLabel(self._legend_text(cfg))
        self._legend_lbl.setFont(QFont("Segoe UI", 8))
        self._legend_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._legend_lbl.setStyleSheet(
            f"QLabel {{ color:{_TEXT_DIM}; background:{_BG_TITLE}; padding:3px 8px;"
            "  border-bottom-left-radius:8px; border-bottom-right-radius:8px; }}"
        )
        return self._legend_lbl

    @staticmethod
    def _legend_text(cfg: Config) -> str:
        return (
            f"{cfg.next_step_hotkey} = Next   "
            f"{cfg.correction_hotkey} = Wrong   "
            f"{cfg.pause_hotkey} = Pause   "
            f"{cfg.floating_window_hotkey} = Icon"
        )

    def _add_shortcuts(self) -> None:
        QShortcut(QKeySequence("Ctrl+N"), self).activated.connect(self.new_session_requested.emit)
        QShortcut(QKeySequence("Ctrl+S"), self).activated.connect(self.save_session_requested.emit)
        QShortcut(QKeySequence("Escape"), self).activated.connect(self.overlay_dismiss_requested.emit)

    # ── Position ──────────────────────────────────────────────────────────────

    def _position_bottom_right(self) -> None:
        screen = QApplication.primaryScreen()
        if screen:
            geom = screen.availableGeometry()
            margin = 16
            self.move(
                geom.right()  - self.width()  - margin,
                geom.bottom() - self.height() - margin,
            )

    # ── Mode switching ────────────────────────────────────────────────────────

    def _to_icon_mode(self) -> None:
        self._panel_pos = self.pos()
        centre = self.geometry().center()
        self._in_panel_mode = False
        self._panel_w.hide()
        self._icon_w.show()
        self.setFixedSize(56, 56)
        self.move(centre.x() - 28, centre.y() - 28)

    def _to_panel_mode(self) -> None:
        self._in_panel_mode = True
        self._icon_w.hide()
        self._panel_w.show()
        self.setFixedSize(360, 540)
        if self._panel_pos is not None:
            self.move(self._panel_pos)
        else:
            self._position_bottom_right()
        self._input.setFocus()

    def toggle_visibility(self) -> None:
        """Ctrl+Shift+Space: toggle between icon and panel mode."""
        if self._in_panel_mode:
            self._to_icon_mode()
        else:
            self._to_panel_mode()

    # ── Public API ────────────────────────────────────────────────────────────

    def add_message(self, role: str, content: str) -> None:
        """Add a message to the chat history and update latest-instruction if assistant."""
        colors = {
            "user":       (_ORANGE,   "You"),
            "assistant":  ("#4EC9B0", "AI"),
            "system":     (_TEXT_DIM, "System"),
            "correction": ("#CE9178", "Correction"),
        }
        color, label = colors.get(role, (_TEXT, role.capitalize()))
        # Escape HTML entities
        safe = content.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        html = (
            f'<span style="color:{color}; font-weight:bold;">{label}:</span>'
            f'&nbsp;<span style="color:{_TEXT};">{safe}</span>'
        )
        self._chat.append(html)
        sb = self._chat.verticalScrollBar()
        sb.setValue(sb.maximum())

        if role == "assistant":
            self._latest_lbl.setText(content)

    def show_status(self, text: str) -> None:
        self._step_lbl.setText(text)

    def update_token_display(self, total_tokens: int) -> None:
        if total_tokens > 0:
            self._token_lbl.setText(f"{total_tokens:,}t")

    def set_processing(self, is_processing: bool) -> None:
        self._send_btn.setEnabled(not is_processing)
        dot = _DOT_BUSY if is_processing else _DOT_READY
        style = f"color:{dot}; background:transparent;"
        self._status_dot.setStyleSheet(style)
        self._icon_dot.setStyleSheet(style)
        if is_processing and not self._streaming_active:
            self._thinking_dots = 0
            self._latest_lbl.setText("Thinking…")
            self._thinking_timer.start()
        else:
            self._thinking_timer.stop()

    def _tick_thinking(self) -> None:
        if not self._streaming_active:
            self._thinking_dots = (self._thinking_dots + 1) % 4
            self._latest_lbl.setText("Thinking" + "." * self._thinking_dots)

    def begin_streaming_message(self) -> None:
        self._thinking_timer.stop()
        self._streaming_text = ""
        self._streaming_active = True
        self._latest_lbl.setText("▋")

    def append_streaming_chunk(self, text: str) -> None:
        if not self._streaming_active:
            self.begin_streaming_message()
        self._streaming_text += text
        self._latest_lbl.setText(self._streaming_text + " ▋")

    def end_streaming_message(self) -> None:
        self._streaming_active = False
        if self._streaming_text:
            self._latest_lbl.setText(self._streaming_text)
        self._streaming_text = ""

    def clear_chat(self) -> None:
        self._chat.clear()
        self._latest_lbl.setText("Waiting for task…")
        self._token_lbl.setText("")
        self._step_lbl.setText("")

    def update_hotkey_tooltips(self, cfg: Config) -> None:
        self._next_btn.setToolTip(f"Next step ({cfg.next_step_hotkey})")
        self._wrong_btn.setToolTip(f"Re-analyze ({cfg.correction_hotkey})")
        self._pause_btn.setToolTip(f"Pause capture ({cfg.pause_hotkey})")
        self._mic_btn.setToolTip(f"Push to talk ({cfg.talk_hotkey})")
        self._legend_lbl.setText(self._legend_text(cfg))

    def set_paused(self, paused: bool) -> None:
        self._pause_btn.setText("▶ Resume" if paused else "⏸ Pause")

    def set_voice_enabled(self, enabled: bool) -> None:
        self._mic_btn.setVisible(enabled)

    def set_listening(self, listening: bool) -> None:
        self._mic_btn.setText("🔴 Listening…" if listening else "🎤 Speak")
        self._mic_btn.setEnabled(not listening)

    # ── Drag-to-move ──────────────────────────────────────────────────────────

    def mousePressEvent(self, event) -> None:
        if event.button() == Qt.MouseButton.LeftButton:
            # Record press position in both modes — drag threshold decides click vs drag
            self._drag_offset = (
                event.globalPosition().toPoint() - self.frameGeometry().topLeft()
            )
            self._drag_started = False
            if self._in_panel_mode:
                # Panel mode: only drag from title bar area (top 32px)
                self._dragging = event.pos().y() <= _TITLE_H
            else:
                # Icon mode: drag from anywhere
                self._dragging = True

    def mouseMoveEvent(self, event) -> None:
        if self._dragging and self._drag_offset is not None:
            delta = event.globalPosition().toPoint() - self.frameGeometry().topLeft() - self._drag_offset
            if not self._drag_started and (abs(delta.x()) + abs(delta.y())) > 5:
                self._drag_started = True
            if self._drag_started:
                self.move(event.globalPosition().toPoint() - self._drag_offset)

    def mouseReleaseEvent(self, event) -> None:
        was_dragging = self._dragging
        drag_moved   = self._drag_started
        self._dragging     = False
        self._drag_offset  = None
        self._drag_started = False
        # Icon mode: pure click (no movement) → expand to panel
        if was_dragging and not drag_moved and not self._in_panel_mode:
            self._to_panel_mode()

    def showEvent(self, event) -> None:
        super().showEvent(event)

    # ── Internal ──────────────────────────────────────────────────────────────

    def _on_send(self) -> None:
        text = self._input.text().strip()
        if text:
            self._input.clear()
            self.message_submitted.emit(text)

    def _on_pause(self) -> None:
        self.pause_toggled.emit()
