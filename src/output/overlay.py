"""Screen overlay renderer for AI Navigator.

Draws arrows, highlights, circles, and subtitle text on top of all windows.
Uses a Qt frameless transparent window that is click-through (input-transparent).

The overlay never intercepts clicks or keystrokes — it is purely visual.
"""

import logging
import math
import sys
from typing import Optional

from PySide6.QtCore import QPoint, QRect, QTimer, Qt
from PySide6.QtGui import QGuiApplication
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

        # Cover the virtual desktop (union of all screens).
        # This ensures the overlay works on any monitor in a multi-monitor setup.
        # The window is input-transparent so covering inactive screens is harmless.
        self._update_geometry()

        # Overlay state
        self._bbox: Optional[tuple[int, int, int, int]] = None  # (x, y, w, h)
        self._overlay_type: str = "none"  # arrow, highlight, circle, none
        self._instruction: str = ""
        self._subtitle_text: str = ""
        self._show_subtitle: bool = False

        # Visual config
        self._color = QColor("#FF6B35")
        self._thickness = 4          # Inner colored stroke
        self._outline_thickness = 8  # White outline drawn underneath for contrast
        self._subtitle_font_size = 18
        self._subtitle_bg_opacity = 180

        # Auto-hide timer
        self._hide_timer = QTimer(self)
        self._hide_timer.setSingleShot(True)
        self._hide_timer.timeout.connect(self.clear)

        # Exclude from screen capture so the overlay does not appear in mss
        # screenshots.  This prevents the screen-change monitor from firing on
        # overlay updates and prevents OCR from reading subtitle text.
        # WDA_EXCLUDEFROMCAPTURE (0x11) is available on Windows 10 2004+.
        self._affinity_set = False

    def showEvent(self, event) -> None:
        """Apply WDA_EXCLUDEFROMCAPTURE the first time the window is shown.

        The HWND is only guaranteed valid after the OS window is created, which
        happens on first show. The affinity must be re-applied after any
        setWindowFlags call that recreates the native window.
        """
        super().showEvent(event)
        if sys.platform == "win32" and not self._affinity_set:
            try:
                import ctypes
                WDA_EXCLUDEFROMCAPTURE = 0x00000011
                hwnd = int(self.winId())
                if ctypes.windll.user32.SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE):
                    self._affinity_set = True
                    logger.debug("Overlay excluded from screen capture (WDA_EXCLUDEFROMCAPTURE)")
                else:
                    logger.warning("SetWindowDisplayAffinity failed — overlay may appear in screenshots")
            except Exception as exc:
                logger.warning("Could not set display affinity: %s", exc)

    def _update_geometry(self) -> None:
        """Set overlay geometry to the virtual desktop union of all screens."""
        virtual = QRect()
        for screen in QGuiApplication.screens():
            virtual = virtual.united(screen.geometry())
        if virtual.isValid():
            self.setGeometry(virtual)

    def _active_screen_rect(self) -> QRect:
        """Return the geometry of the screen containing the current bbox, or primary."""
        if self._bbox:
            cx = self._bbox[0] + self._bbox[2] // 2
            cy = self._bbox[1] + self._bbox[3] // 2
            for screen in QGuiApplication.screens():
                if screen.geometry().contains(cx, cy):
                    return screen.geometry()
        primary = QApplication.primaryScreen()
        return primary.geometry() if primary else self.geometry()

    def set_colors(self, color: str, thickness: int = 4) -> None:
        """Update overlay colors from config."""
        self._color = QColor(color)
        self._thickness = thickness
        self._outline_thickness = thickness * 2 + 2

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

    def _make_pen(self, color: QColor, width: int, round_cap: bool = False) -> QPen:
        """Create a QPen safely using the two-arg constructor + setters.

        Avoids the multi-arg QPen(QColor, width, style, cap) constructor which
        is not valid in PySide6 — only QPen(QBrush, width, ...) accepts extra args.
        """
        pen = QPen(color)
        pen.setWidth(width)
        if round_cap:
            pen.setCapStyle(Qt.PenCapStyle.RoundCap)
        return pen

    def _white_pen(self, width: int) -> QPen:
        return self._make_pen(QColor(255, 255, 255, 220), width, round_cap=True)

    def _draw_arrow(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw an arrow pointing to the target bbox.

        White outline drawn first, colored stroke on top — visible on any background.
        """
        cx = x + w // 2
        cy = y + h // 2
        # Use the active screen height for arrow placement decisions
        screen_rect = self._active_screen_rect()
        geo = self.geometry()
        screen_h = screen_rect.height()
        local_cy = cy - (screen_rect.y() - geo.y())  # cy relative to screen top
        offset = 130

        if local_cy > screen_h // 2:
            ox, oy = cx, max(0, y - offset)
        else:
            ox, oy = cx, min(screen_h, y + h + offset)

        # --- White outline pass ---
        painter.setPen(self._white_pen(self._outline_thickness))
        painter.drawLine(QPoint(ox, oy), QPoint(cx, cy))
        painter.drawRect(x - 3, y - 3, w + 6, h + 6)
        self._draw_arrowhead(painter, ox, oy, cx, cy, size=22, color=QColor(255, 255, 255, 220))

        # --- Colored pass ---
        painter.setPen(self._make_pen(self._color, self._thickness, round_cap=True))
        painter.drawLine(QPoint(ox, oy), QPoint(cx, cy))
        painter.setPen(self._make_pen(self._color, self._thickness))
        painter.drawRect(x - 3, y - 3, w + 6, h + 6)
        self._draw_arrowhead(painter, ox, oy, cx, cy, size=16, color=self._color)

    def _draw_arrowhead(
        self,
        painter: QPainter,
        x1: int,
        y1: int,
        x2: int,
        y2: int,
        size: int = 16,
        color: Optional[QColor] = None,
    ) -> None:
        """Draw a filled arrowhead at (x2, y2) pointing from (x1, y1).

        Saves and restores painter pen so callers are unaffected.
        """
        if color is None:
            color = self._color
        angle = math.atan2(y2 - y1, x2 - x1)

        p1 = QPoint(
            int(x2 - size * math.cos(angle - math.pi / 6)),
            int(y2 - size * math.sin(angle - math.pi / 6)),
        )
        p2 = QPoint(
            int(x2 - size * math.cos(angle + math.pi / 6)),
            int(y2 - size * math.sin(angle + math.pi / 6)),
        )

        saved_pen = painter.pen()
        triangle = QPolygon([QPoint(x2, y2), p1, p2])
        painter.setBrush(QBrush(color))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.drawPolygon(triangle)
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.setPen(saved_pen)  # Restore pen for caller

    def _draw_highlight(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw a colored rectangle highlight. White outline for contrast."""
        fill_color = QColor(self._color)
        fill_color.setAlpha(70)
        painter.fillRect(x, y, w, h, fill_color)

        painter.setPen(self._white_pen(self._outline_thickness))
        painter.drawRect(x - 2, y - 2, w + 4, h + 4)

        painter.setPen(self._make_pen(self._color, self._thickness))
        painter.drawRect(x, y, w, h)

    def _draw_circle(self, painter: QPainter, x: int, y: int, w: int, h: int) -> None:
        """Draw a circle around the target center. White outline for contrast."""
        cx = x + w // 2
        cy = y + h // 2
        radius = max(w, h) // 2 + 18

        painter.setPen(self._white_pen(self._outline_thickness))
        painter.drawEllipse(QPoint(cx, cy), radius + 2, radius + 2)

        painter.setPen(self._make_pen(self._color, self._thickness))
        painter.drawEllipse(QPoint(cx, cy), radius, radius)

    def _draw_subtitle(self, painter: QPainter, text: str) -> None:
        """Draw subtitle text at the bottom-center of the active screen."""
        font = QFont("Segoe UI", self._subtitle_font_size)
        font.setBold(True)
        painter.setFont(font)

        # Use the active screen rect so subtitles appear on the right monitor
        screen_rect = self._active_screen_rect()
        geo = self.geometry()
        # Convert screen-absolute coords to overlay-local coords
        sx = screen_rect.x() - geo.x()
        sy = screen_rect.y() - geo.y()
        sw = screen_rect.width()
        sh = screen_rect.height()

        # Calculate text bounds
        metrics = painter.fontMetrics()
        max_width = int(sw * 0.7)
        text_rect = metrics.boundingRect(
            QRect(0, 0, max_width, 0),
            Qt.TextFlag.TextWordWrap | Qt.AlignmentFlag.AlignCenter,
            text,
        )

        # Position subtitle away from the target element.
        # If the bbox is in the bottom half of the screen (or no bbox), place at top.
        # If it's in the top half, place at bottom. Prevents subtitle from covering the target.
        margin = 40
        x = sx + (sw - text_rect.width()) // 2
        if self._bbox:
            _, ey, _, eh = self._bbox
            element_center_y = ey + eh // 2
            screen_mid = sy + sh // 2
            if element_center_y >= screen_mid:
                # Target in bottom half → subtitle at top
                y = sy + margin
            else:
                # Target in top half → subtitle at bottom
                y = sy + sh - text_rect.height() - margin
        else:
            y = sy + sh - text_rect.height() - margin

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
