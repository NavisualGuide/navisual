"""Floating quick-input window for AI Navigator.

A small, always-on-top panel activated by hotkey. Provides quick text input,
correction button, pause control, and step navigation without switching
to the main chat window.
"""

import logging

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QFont
from PySide6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)


class FloatingWindow(QWidget):
    """Floating quick-input panel.

    Activated via hotkey (default: Ctrl+Shift+Space).
    Provides quick access to common actions without switching windows.
    """

    # Signals
    message_submitted = Signal(str)
    correction_requested = Signal()
    pause_toggled = Signal()
    next_step_requested = Signal()

    def __init__(self) -> None:
        super().__init__()

        self.setWindowTitle("AI Navigator")
        self.setWindowFlags(
            Qt.WindowType.FramelessWindowHint
            | Qt.WindowType.WindowStaysOnTopHint
            | Qt.WindowType.Tool
        )
        self.setFixedSize(380, 140)

        self._is_paused = False
        self._dragging = False
        self._drag_offset = None

        self._setup_ui()
        self._position_bottom_right()

    def _setup_ui(self) -> None:
        """Create the floating window UI."""
        layout = QVBoxLayout(self)
        layout.setContentsMargins(10, 8, 10, 8)
        layout.setSpacing(6)

        self.setStyleSheet(
            "QWidget { background-color: #2d2d2d; color: #d4d4d4; border-radius: 8px; }"
        )

        # Title bar with close button
        title_row = QHBoxLayout()
        title = QLabel("AI Navigator")
        title.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        title.setStyleSheet("color: #FF6B35;")
        title_row.addWidget(title)

        title_row.addStretch()

        close_btn = QPushButton("×")
        close_btn.setFixedSize(20, 20)
        close_btn.setStyleSheet(
            "QPushButton { background: transparent; color: #888; border: none; font-size: 16px; }"
            "QPushButton:hover { color: #fff; }"
        )
        close_btn.clicked.connect(self.hide)
        title_row.addWidget(close_btn)
        layout.addLayout(title_row)

        # Quick input
        input_layout = QHBoxLayout()
        self._input_field = QLineEdit()
        self._input_field.setFont(QFont("Segoe UI", 10))
        self._input_field.setPlaceholderText("Quick message...")
        self._input_field.setStyleSheet(
            "QLineEdit { background: #1e1e1e; border: 1px solid #444; "
            "border-radius: 4px; padding: 4px 8px; }"
        )
        self._input_field.returnPressed.connect(self._on_send)
        input_layout.addWidget(self._input_field, stretch=1)

        send_btn = QPushButton("→")
        send_btn.setFixedSize(30, 28)
        send_btn.setStyleSheet(
            "QPushButton { background: #FF6B35; color: white; border: none; "
            "border-radius: 4px; font-weight: bold; font-size: 14px; }"
            "QPushButton:hover { background: #E55A2B; }"
        )
        send_btn.clicked.connect(self._on_send)
        input_layout.addWidget(send_btn)
        layout.addLayout(input_layout)

        # Action buttons
        btn_layout = QHBoxLayout()
        btn_layout.setSpacing(6)

        btn_style = (
            "QPushButton {{ background: {bg}; color: {fg}; border: none; "
            "border-radius: 4px; padding: 6px 10px; font-size: 10px; font-weight: bold; }}"
            "QPushButton:hover {{ background: {hover}; }}"
        )

        # Correction button
        self._correction_btn = QPushButton("✗ Wrong")
        self._correction_btn.setStyleSheet(
            btn_style.format(bg="#C62828", fg="white", hover="#B71C1C")
        )
        self._correction_btn.setToolTip("Re-analyze (Ctrl+Shift+X)")
        self._correction_btn.clicked.connect(self.correction_requested.emit)
        btn_layout.addWidget(self._correction_btn)

        # Pause button
        self._pause_btn = QPushButton("⏸ Pause")
        self._pause_btn.setStyleSheet(
            btn_style.format(bg="#F57F17", fg="white", hover="#E65100")
        )
        self._pause_btn.setToolTip("Pause capture (Ctrl+Shift+P)")
        self._pause_btn.clicked.connect(self._on_pause)
        btn_layout.addWidget(self._pause_btn)

        # Next step button
        self._next_btn = QPushButton("→ Next")
        self._next_btn.setStyleSheet(
            btn_style.format(bg="#2E7D32", fg="white", hover="#1B5E20")
        )
        self._next_btn.setToolTip("Next step (Ctrl+Shift+N)")
        self._next_btn.clicked.connect(self.next_step_requested.emit)
        btn_layout.addWidget(self._next_btn)

        layout.addLayout(btn_layout)

    def _position_bottom_right(self) -> None:
        """Position window at bottom-right of primary screen."""
        from PySide6.QtWidgets import QApplication

        screen = QApplication.primaryScreen()
        if screen:
            geo = screen.availableGeometry()
            x = geo.right() - self.width() - 20
            y = geo.bottom() - self.height() - 20
            self.move(x, y)

    def _on_send(self) -> None:
        """Handle message submission."""
        text = self._input_field.text().strip()
        if text:
            self._input_field.clear()
            self.message_submitted.emit(text)

    def _on_pause(self) -> None:
        """Toggle pause state."""
        self._is_paused = not self._is_paused
        if self._is_paused:
            self._pause_btn.setText("▶ Resume")
        else:
            self._pause_btn.setText("⏸ Pause")
        self.pause_toggled.emit()

    def set_paused(self, paused: bool) -> None:
        """Update the pause button state externally."""
        self._is_paused = paused
        self._pause_btn.setText("▶ Resume" if paused else "⏸ Pause")

    # --- Dragging support ---
    def mousePressEvent(self, event) -> None:
        if event.button() == Qt.MouseButton.LeftButton:
            self._dragging = True
            self._drag_offset = event.globalPosition().toPoint() - self.frameGeometry().topLeft()

    def mouseMoveEvent(self, event) -> None:
        if self._dragging and self._drag_offset:
            self.move(event.globalPosition().toPoint() - self._drag_offset)

    def mouseReleaseEvent(self, event) -> None:
        self._dragging = False
        self._drag_offset = None

    def toggle_visibility(self) -> None:
        """Toggle window visibility (for hotkey)."""
        if self.isVisible():
            self.hide()
        else:
            self.show()
            self._input_field.setFocus()
            self.raise_()
