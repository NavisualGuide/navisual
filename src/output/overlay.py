"""Screen overlay renderer for AI Navigator.

Draws arrows, highlights, circles, and subtitle text on top of all windows.
Uses a Qt frameless transparent window that is click-through (input-transparent).

The overlay never intercepts clicks or keystrokes — it is purely visual.
"""

import logging
import math
from typing import Optional

from PySide6.QtCore import QPoint, QRect, QTimer, Qt
from PySide6.QtGui import QBrush, QColor, QFont, QPainter, QPainterPath, QPen, QPolygon
from PySide6.QtWidgets import QApplication, QWidget

logger = logging.getLogger(__name__)


class OverlayWindow(QWidget):
    """Transparent, click-through overlay for navigation indicators.

    Renders arrows, highlights, circles, and subtitles over the user's screen.
    Always on top, fully transparent to mouse events.
    """

    def __init__(self) -> None:
        super().__init__()

        # Window flags: frameless, always on top, tool window (no taskbar)
        self.setWindowFlags(
            Qt.WindowType.FramelessWindowHint
            | Qt.WindowType.WindowStaysOnTopHint
            | Qt.WindowType.Tool
            | Qt.WindowType.WindowTransparentForInput
        )
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground, True)
        self.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents, True)
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating, True)

        # Cover the full primary screen
        screen = QApplication.primaryScreen()
        if screen:
            geo = screen.geometry()
            self.setGeometry(geo)

        # Overlay state
        self._bbox: Optional[tuple[int, int, int, int]] = None  # (x, y, w, h)
        self._overlay_type: str = "none"  # arrow, highlight, circle, none
        self._instruction: str = ""
        self._subtitle_text: str = ""
        self._show_subtitle: bool = False

        # Visual config
        self._color = QColor("#FF6B35")
        self._thickness = 3
        self._subtitle_font_size = 18
        self._subtitle_bg_opacity = 180

        # Auto-hide timer
        self._hide_timer = QTimer(self)
        self._hide_timer.setSingleShot(True)
        self._hide_timer.timeout.connect(self.clear)

    def set_colors(self, color: str, thickness: int = 3) -> None:
        """Update overlay colors from config."""
        self._color = QColor(color)
        self._thickness = thickness

    def set_subtitle_style(self, font_size: int = 18, bg_opacity: int = 180) -> None:
        """Update subtitle visual style."""
        self._subtitle_font_size = font_size
        self._subtitle_bg_opacity = bg_opacity

    def show_overlay(
        self,
        bbox: tuple[int, int, int, int],
        overlay_type: str = "arrow",
        instruction: str = "",
        auto_hide_ms: int = 0,
    ) -> None:
        """Show an overlay indicator at the target bounding box.

        Args:
            bbox: Target position (x, y, width, height) in screen coordinates.
            overlay_type: "arrow", "highlight", "circle", or "none".
            instruction: Subtitle text to display.
            auto_hide_ms: Auto-hide after this many ms (0 = no auto-hide).
        """
        self._bbox = bbox
        self._overlay_type = overlay_type
        self._instruction = instruction
        self._subtitle_text = instruction
        self._show_subtitle = bool(instruction)

        self.show()
        self.update()

        if auto_hide_ms > 0:
            self._hide_timer.start(auto_hide_ms)

    def show_subtitle(self, text: str, auto_hide_ms: int = 0) -> None:
        """Show subtitle-only instruction (when element location fails)."""
        self._bbox = None
        self._overlay_type = "none"
        self._subtitle_text = text
        self._show_subtitle = True

        self.show()
        self.update()

        if auto_hide_ms > 0:
            self._hide_timer.start(auto_hide_ms)

    def clear(self) -> None:
        """Clear all overlay elements."""
        self._bbox = None
        self._overlay_type = "none"
        self._subtitle_text = ""
        self._show_subtitle = False
        self._hide_timer.stop()
        self.update()

    def paintEvent(self, event) -> None:
        """Render the overlay elements."""
        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)

        try:
            if self._bbox and self._overlay_type != "none":
                x, y, w, h = self._bbox
                # Adjust for window position (overlay covers full screen from 0,0)
                geo = self.geometry()
                x -= geo.x()
                y -= geo.y()

                if self._overlay_type == "arrow":
                    self._draw_arrow(painter, x, y, w, h)
                elif self._overlay_type == "highlight":
                    self._draw_highlight(painter, x, y, w, h)
                elif self._overlay_type == "circle":
                    self._draw_circle(painter, x, y, w, h)

            if self._show_subtitle and self._subtitle_text:
                self._draw_subtitle(painter, self._subtitle_text)

        finally:
            painter.end()

    def _draw_arrow(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw an arrow pointing to the target bbox."""
        pen = QPen(self._color, self._thickness)
        painter.setPen(pen)

        # Target center
        cx = x + w // 2
        cy = y + h // 2

        # Arrow origin: offset from target (pick the direction with most space)
        screen_w = self.width()
        screen_h = self.height()
        offset = 120

        # Choose arrow origin based on available space
        if cy > screen_h // 2:
            # Target is in bottom half — arrow comes from above
            ox, oy = cx, max(0, y - offset)
        else:
            # Target is in top half — arrow comes from below
            ox, oy = cx, min(screen_h, y + h + offset)

        # Draw the line
        painter.drawLine(QPoint(ox, oy), QPoint(cx, cy))

        # Draw arrowhead
        self._draw_arrowhead(painter, ox, oy, cx, cy)

        # Draw highlight box around target
        highlight_pen = QPen(self._color, 2)
        painter.setPen(highlight_pen)
        painter.drawRect(x - 2, y - 2, w + 4, h + 4)

    def _draw_arrowhead(
        self, painter: QPainter, x1: int, y1: int, x2: int, y2: int
    ) -> None:
        """Draw an arrowhead at the end of a line (at x2, y2)."""
        arrow_size = 14
        angle = math.atan2(y2 - y1, x2 - x1)

        p1 = QPoint(
            int(x2 - arrow_size * math.cos(angle - math.pi / 6)),
            int(y2 - arrow_size * math.sin(angle - math.pi / 6)),
        )
        p2 = QPoint(
            int(x2 - arrow_size * math.cos(angle + math.pi / 6)),
            int(y2 - arrow_size * math.sin(angle + math.pi / 6)),
        )

        triangle = QPolygon([QPoint(x2, y2), p1, p2])
        painter.setBrush(QBrush(self._color))
        painter.drawPolygon(triangle)

    def _draw_highlight(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw a colored rectangle highlight around the target."""
        # Semi-transparent fill
        fill_color = QColor(self._color)
        fill_color.setAlpha(40)
        painter.fillRect(x, y, w, h, fill_color)

        # Solid border
        pen = QPen(self._color, self._thickness)
        painter.setPen(pen)
        painter.drawRect(x, y, w, h)

    def _draw_circle(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw a circle around the target center."""
        cx = x + w // 2
        cy = y + h // 2
        radius = max(w, h) // 2 + 15

        pen = QPen(self._color, self._thickness)
        painter.setPen(pen)
        painter.drawEllipse(QPoint(cx, cy), radius, radius)

    def _draw_subtitle(self, painter: QPainter, text: str) -> None:
        """Draw subtitle text at the bottom-center of the screen."""
        font = QFont("Segoe UI", self._subtitle_font_size)
        font.setBold(True)
        painter.setFont(font)

        # Calculate text bounds
        metrics = painter.fontMetrics()
        # Wrap long text
        max_width = int(self.width() * 0.7)
        text_rect = metrics.boundingRect(
            QRect(0, 0, max_width, 0),
            Qt.TextFlag.TextWordWrap | Qt.AlignmentFlag.AlignCenter,
            text,
        )

        # Position at bottom-center
        margin = 40
        x = (self.width() - text_rect.width()) // 2
        y = self.height() - text_rect.height() - margin

        # Draw background
        bg_rect = QRect(
            x - 16, y - 8,
            text_rect.width() + 32, text_rect.height() + 16,
        )
        bg_color = QColor(0, 0, 0, self._subtitle_bg_opacity)
        painter.fillRect(bg_rect, bg_color)

        # Draw border
        border_color = QColor(self._color)
        border_color.setAlpha(200)
        painter.setPen(QPen(border_color, 2))
        painter.drawRect(bg_rect)

        # Draw text
        painter.setPen(QPen(QColor(255, 255, 255)))
        draw_rect = QRect(x, y, text_rect.width(), text_rect.height())
        painter.drawText(
            draw_rect,
            Qt.TextFlag.TextWordWrap | Qt.AlignmentFlag.AlignCenter,
            text,
        )
