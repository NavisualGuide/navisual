"""Main chat window for AI Navigator.

PySide6-based chat interface for user interaction. Displays conversation
history, accepts text prompts, and shows session status.
"""

import logging
from typing import Optional

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QAction, QFont, QTextCharFormat, QColor, QTextCursor
from PySide6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMainWindow,
    QMenuBar,
    QPushButton,
    QStatusBar,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)


class MainWindow(QMainWindow):
    """Main chat window for AI Navigator.

    Provides:
    - Scrollable conversation display
    - Text input with send button
    - Status bar (connection, token usage, session info)
    - Menu bar (session management)
    """

    # Signals
    message_submitted = Signal(str)
    new_session_requested = Signal()
    open_session_requested = Signal()
    save_session_requested = Signal()

    def __init__(self) -> None:
        super().__init__()

        self.setWindowTitle("AI Navigator")
        self.setMinimumSize(500, 600)
        self.resize(600, 750)

        self._setup_menu_bar()
        self._setup_central_widget()
        self._setup_status_bar()

    def _setup_menu_bar(self) -> None:
        """Create the menu bar with session actions."""
        menu_bar = self.menuBar()

        # File menu
        file_menu = menu_bar.addMenu("&File")

        new_action = QAction("&New Session", self)
        new_action.setShortcut("Ctrl+N")
        new_action.triggered.connect(self.new_session_requested.emit)
        file_menu.addAction(new_action)

        open_action = QAction("&Open Session...", self)
        open_action.setShortcut("Ctrl+O")
        open_action.triggered.connect(self.open_session_requested.emit)
        file_menu.addAction(open_action)

        save_action = QAction("&Save Session", self)
        save_action.setShortcut("Ctrl+S")
        save_action.triggered.connect(self.save_session_requested.emit)
        file_menu.addAction(save_action)

        file_menu.addSeparator()

        quit_action = QAction("&Quit", self)
        quit_action.setShortcut("Ctrl+Q")
        quit_action.triggered.connect(self.close)
        file_menu.addAction(quit_action)

    def _setup_central_widget(self) -> None:
        """Create the chat display and input area."""
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)
        layout.setContentsMargins(10, 10, 10, 10)
        layout.setSpacing(8)

        # Header
        header = QLabel("AI Navigator")
        header.setFont(QFont("Segoe UI", 16, QFont.Weight.Bold))
        header.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(header)

        tagline = QLabel("The AI guides, never overrides.")
        tagline.setFont(QFont("Segoe UI", 10))
        tagline.setAlignment(Qt.AlignmentFlag.AlignCenter)
        tagline.setStyleSheet("color: #888;")
        layout.addWidget(tagline)

        # Chat display (read-only)
        self._chat_display = QTextEdit()
        self._chat_display.setReadOnly(True)
        self._chat_display.setFont(QFont("Segoe UI", 11))
        self._chat_display.setStyleSheet(
            "QTextEdit { background-color: #1e1e1e; color: #d4d4d4; "
            "border: 1px solid #333; border-radius: 4px; padding: 8px; }"
        )
        layout.addWidget(self._chat_display, stretch=1)

        # Input area
        input_layout = QHBoxLayout()
        input_layout.setSpacing(6)

        self._input_field = QLineEdit()
        self._input_field.setFont(QFont("Segoe UI", 11))
        self._input_field.setPlaceholderText("Type your request... (e.g., 'Help me buy a USB-C cable on Amazon')")
        self._input_field.setStyleSheet(
            "QLineEdit { background-color: #2d2d2d; color: #d4d4d4; "
            "border: 1px solid #444; border-radius: 4px; padding: 8px; }"
        )
        self._input_field.returnPressed.connect(self._on_send)
        input_layout.addWidget(self._input_field, stretch=1)

        self._send_button = QPushButton("Send")
        self._send_button.setFont(QFont("Segoe UI", 11))
        self._send_button.setStyleSheet(
            "QPushButton { background-color: #FF6B35; color: white; "
            "border: none; border-radius: 4px; padding: 8px 20px; font-weight: bold; }"
            "QPushButton:hover { background-color: #E55A2B; }"
            "QPushButton:disabled { background-color: #555; }"
        )
        self._send_button.clicked.connect(self._on_send)
        input_layout.addWidget(self._send_button)

        layout.addLayout(input_layout)

    def _setup_status_bar(self) -> None:
        """Create the status bar with connection and usage info."""
        self._status_bar = QStatusBar()
        self.setStatusBar(self._status_bar)

        self._status_label = QLabel("Ready")
        self._status_bar.addWidget(self._status_label, stretch=1)

        self._token_label = QLabel("Tokens: 0")
        self._status_bar.addPermanentWidget(self._token_label)

    def _on_send(self) -> None:
        """Handle send button click or Enter key."""
        text = self._input_field.text().strip()
        if text:
            self._input_field.clear()
            self.message_submitted.emit(text)

    def add_message(self, role: str, content: str) -> None:
        """Add a message to the chat display.

        Args:
            role: "user", "assistant", "system", or "correction"
            content: Message text.
        """
        cursor = self._chat_display.textCursor()
        cursor.movePosition(QTextCursor.MoveOperation.End)

        # Role header
        header_fmt = QTextCharFormat()
        header_fmt.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))

        if role == "user":
            header_fmt.setForeground(QColor("#4FC3F7"))
            prefix = "You"
        elif role == "assistant":
            header_fmt.setForeground(QColor("#FF6B35"))
            prefix = "Navigator"
        elif role == "correction":
            header_fmt.setForeground(QColor("#FFD54F"))
            prefix = "Correction"
        else:
            header_fmt.setForeground(QColor("#888888"))
            prefix = "System"

        cursor.insertText(f"\n{prefix}:\n", header_fmt)

        # Content
        content_fmt = QTextCharFormat()
        content_fmt.setFont(QFont("Segoe UI", 11))
        content_fmt.setForeground(QColor("#D4D4D4"))
        cursor.insertText(f"{content}\n", content_fmt)

        # Scroll to bottom
        self._chat_display.verticalScrollBar().setValue(
            self._chat_display.verticalScrollBar().maximum()
        )

    def show_status(self, text: str) -> None:
        """Update the status bar text."""
        self._status_label.setText(text)

    def update_token_display(self, total_tokens: int) -> None:
        """Update the token usage display."""
        self._token_label.setText(f"Tokens: {total_tokens:,}")

    def set_processing(self, is_processing: bool) -> None:
        """Enable/disable input during API calls."""
        self._input_field.setEnabled(not is_processing)
        self._send_button.setEnabled(not is_processing)
        if is_processing:
            self._send_button.setText("Thinking...")
            self.show_status("Processing...")
        else:
            self._send_button.setText("Send")
            self.show_status("Ready")

    def clear_chat(self) -> None:
        """Clear the chat display."""
        self._chat_display.clear()

    def set_input_text(self, text: str) -> None:
        """Set the input field text (for history navigation)."""
        self._input_field.setText(text)
        self._input_field.setFocus()
