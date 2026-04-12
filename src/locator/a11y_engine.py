"""OS Accessibility API engine for AI Navigator.

PRIMARY element locator. Uses Windows UI Automation (UIA) to query
the foreground application's widget tree for element name, role,
and bounding box. Operates in < 5ms for browser windows.

On macOS (v0.3+): will use AXUIElement.
On Linux (v0.4+): will use AT-SPI2.
"""

import logging
import os
import sys
from typing import Optional

from pydantic import BaseModel

logger = logging.getLogger(__name__)

# Role mapping from our schema to UIA ControlType names
_ROLE_TO_CONTROL_TYPE: dict[str, str] = {
    "button": "ButtonControl",
    "tab": "TabItemControl",
    "link": "HyperlinkControl",
    "textbox": "EditControl",
    "menuitem": "MenuItemControl",
    "checkbox": "CheckBoxControl",
    "radio": "RadioButtonControl",
    "combobox": "ComboBoxControl",
    "slider": "SliderControl",
    "image": "ImageControl",
    "heading": "TextControl",
    "other": None,  # No filter
}


class A11yResult(BaseModel):
    """Result from an accessibility API query."""

    bbox: tuple[int, int, int, int]  # (x, y, width, height)
    name: str
    role: str
    confidence: float = 1.0  # A11y results are always high confidence


class A11yEngine:
    """Windows UI Automation element locator.

    Queries the foreground window's UIA tree to find elements by name and role.
    This is the PRIMARY locator strategy for browser tasks (< 5ms).
    """

    def __init__(self) -> None:
        self._available = False
        if sys.platform == "win32":
            try:
                import uiautomation  # noqa: F401

                self._available = True
                logger.info("A11yEngine: Windows UIA initialized")
            except ImportError:
                logger.warning("A11yEngine: uiautomation not installed, A11y disabled")
        else:
            logger.info(
                "A11yEngine: platform %s not yet supported (macOS v0.3, Linux v0.4)",
                sys.platform,
            )

    @property
    def is_available(self) -> bool:
        return self._available

    def find_element(
        self,
        target_text: str,
        target_role: Optional[str] = None,
        timeout_ms: int = 100,
    ) -> Optional[A11yResult]:
        """Find a UI element by its text label and optional role.

        Args:
            target_text: Exact or partial text label to find.
            target_role: UI role (button, tab, link, etc.) to narrow the search.
            timeout_ms: Maximum search time in milliseconds.

        Returns:
            A11yResult with bounding box, or None if not found.
        """
        if not self._available:
            return None

        try:
            return self._find_via_uia(target_text, target_role, timeout_ms)
        except Exception as e:
            logger.debug("A11y lookup failed: %s", e)
            return None

    def _find_via_uia(
        self,
        target_text: str,
        target_role: Optional[str],
        timeout_ms: int,
    ) -> Optional[A11yResult]:
        """Perform the actual UIA search."""
        import ctypes
        import uiautomation as auto

        # Set search timeout
        auto.SetGlobalSearchTimeout(timeout_ms / 1000.0)

        # Build search conditions
        target_lower = target_text.lower()

        # Determine the control type to search for
        control_type_name = None
        if target_role and target_role in _ROLE_TO_CONTROL_TYPE:
            control_type_name = _ROLE_TO_CONTROL_TYPE[target_role]

        # Determine which window(s) to search.
        # If the foreground window is our own process (e.g. user just clicked the
        # Next button in AI Navigator), GetForegroundControl() would return our panel
        # and we'd never find the target in the user's app.
        # In that case, walk the desktop's top-level windows and search each one that
        # belongs to a different process.
        our_pid = os.getpid()
        foreground_hwnd = ctypes.windll.user32.GetForegroundWindow()
        fg_pid = ctypes.c_ulong(0)
        ctypes.windll.user32.GetWindowThreadProcessId(foreground_hwnd, ctypes.byref(fg_pid))
        foreground_is_ours = (fg_pid.value == our_pid)

        if not foreground_is_ours:
            # Normal case: search the foreground window.
            foreground = auto.GetForegroundControl()
            if foreground is None:
                logger.debug("No foreground window found")
                return None
            search_roots = [foreground]
        else:
            # Our panel is focused — search all other top-level windows so we can
            # still locate elements in the user's target application.
            logger.debug("A11y: foreground is AI Navigator, searching all other windows")
            desktop = auto.GetRootControl()
            search_roots = []
            try:
                for child in desktop.GetChildren():
                    try:
                        if child.ProcessId != our_pid:
                            search_roots.append(child)
                    except Exception:
                        continue
            except Exception:
                pass
            if not search_roots:
                return None

        for root in search_roots:
            # Strategy 1: Direct name search
            result = self._search_by_name(root, target_lower, control_type_name, auto)
            if result:
                return result
            # Strategy 2: Search in descendants (broader, slightly slower)
            result = self._search_descendants(root, target_lower, control_type_name, auto)
            if result:
                return result

        return None

    def _search_by_name(
        self,
        root,
        target_lower: str,
        control_type_name: Optional[str],
        auto,
    ) -> Optional[A11yResult]:
        """Search for element by exact name using uiautomation's Control() search."""
        try:
            # uiautomation uses Control(Name=...) style, not COM PropertyCondition
            kwargs = {"searchDepth": 10, "Name": target_lower}
            if control_type_name:
                # e.g. root.ButtonControl(Name=...) via getattr
                finder = getattr(root, control_type_name, None)
                if finder:
                    element = finder(**{"searchDepth": 10, "Name": target_lower})
                    if element and element.Exists(0):
                        result = self._element_to_result(element, auto)
                        if result:
                            return result
            # Generic Control search (case-insensitive via RegexName)
            import re
            pattern = "(?i)^" + re.escape(target_lower) + "$"
            element = root.Control(searchDepth=10, RegexName=pattern)
            if element and element.Exists(0):
                if self._validate_element(element, control_type_name, auto):
                    return self._element_to_result(element, auto)
        except Exception:
            pass
        return None

    def _search_descendants(
        self,
        root,
        target_lower: str,
        control_type_name: Optional[str],
        auto,
    ) -> Optional[A11yResult]:
        """Walk the UIA tree searching for partial name matches.

        Uses uiautomation RegexName for substring match first (fast path),
        then falls back to manual tree walk for non-ASCII or complex patterns.

        Two passes when a control_type is specified:
        1. Role-specific search (fast, precise).
        2. Role-agnostic search (catches elements with unexpected UIA roles,
           e.g. TurboTax "Continue" rendered as HyperlinkControl instead of
           ButtonControl).
        """
        import re

        # Use anchored exact match — same as fast path — so "Insert" does NOT
        # match "Insert Space", "Insert Row", etc. (substring false-positives).
        pattern = "(?i)^" + re.escape(target_lower) + "$"

        def _try_regex(ctype: Optional[str]) -> Optional[A11yResult]:
            """Attempt a RegexName search for the given control type (or any)."""
            try:
                if ctype:
                    finder = getattr(root, ctype, None)
                    if finder:
                        element = finder(searchDepth=12, RegexName=pattern)
                        if element and element.Exists(0):
                            result = self._element_to_result(element, auto)
                            if result:
                                return result
                element = root.Control(searchDepth=12, RegexName=pattern)
                if element and element.Exists(0):
                    result = self._element_to_result(element, auto)
                    if result:
                        return result
            except Exception:
                pass
            return None

        # Pass 1: role-specific (if a role was requested)
        result = _try_regex(control_type_name)
        if result:
            return result

        # Pass 2: role-agnostic fallback (catches wrong-role elements)
        if control_type_name:
            result = _try_regex(None)
            if result:
                return result

        # Slow path: manual walk (catches edge cases the regex search misses).
        # Capped at 200 ms wall-clock time so a large UIA tree (TurboTax, Office)
        # never blocks the Qt main thread long enough to freeze the UI.
        import time as _time
        deadline = _time.monotonic() + 0.20
        try:
            children = self._get_all_controls(root, auto, max_depth=8, deadline=deadline)
            for control in children:
                try:
                    name = control.Name
                    if not name:
                        continue
                    name_lower = name.lower()
                    # Skip elements whose name is much longer than target — these are
                    # container titles (e.g. browser tab "Amazon.ca: USB-C Cable...") that
                    # happen to contain the target as a substring but are not the real element.
                    if len(name) > len(target_lower) * 4:
                        continue
                    if target_lower in name_lower or name_lower in target_lower:
                        if self._validate_element(control, None, auto):  # role-agnostic
                            return self._element_to_result(control, auto)
                except Exception:
                    continue
        except Exception:
            pass
        return None

    def _get_all_controls(self, root, auto, max_depth: int = 6, deadline: float = 0.0) -> list:
        """Recursively get all controls up to max_depth."""
        controls = []
        self._collect_controls(root, controls, auto, depth=0, max_depth=max_depth, deadline=deadline)
        return controls

    def _collect_controls(
        self, element, controls: list, auto, depth: int, max_depth: int, deadline: float = 0.0
    ) -> None:
        """Recursively collect UI controls, stopping at max_depth or deadline."""
        if depth >= max_depth:
            return
        import time as _time
        if deadline and _time.monotonic() > deadline:
            return
        try:
            children = element.GetChildren()
            if children:
                for child in children:
                    controls.append(child)
                    self._collect_controls(child, controls, auto, depth + 1, max_depth, deadline)
        except Exception:
            pass

    def _validate_element(self, element, control_type_name: Optional[str], auto) -> bool:
        """Check if element matches the required control type.

        Always rejects container/window elements (they match by substring but
        are not interactive UI elements the user can act on).
        """
        try:
            actual_type = element.ControlTypeName
            # Never return window/titlebar/pane containers as located elements
            if actual_type in ("WindowControl", "TitleBarControl", "PaneControl"):
                return False
            if control_type_name:
                return actual_type == control_type_name
            return True
        except Exception:
            return True  # Accept if we can't check the type

    def _element_to_result(self, element, auto) -> Optional[A11yResult]:
        """Convert a UIA element to an A11yResult."""
        try:
            rect = element.BoundingRectangle
            if rect.width() <= 0 or rect.height() <= 0:
                return None

            x, y, w, h = rect.left, rect.top, rect.width(), rect.height()

            # Reject elements with obviously bogus coordinates.  The UIA tree can
            # contain virtual / off-screen elements from minimised or ghost windows
            # whose BoundingRectangle is set to something like (-31000, -31000).
            # No real multi-monitor desktop spans beyond ±10 000 px on either axis.
            if abs(x) > 10_000 or abs(y) > 10_000:
                logger.debug(
                    "A11y: rejected off-screen element '%s' at (%d, %d, %d, %d)",
                    element.Name, x, y, w, h,
                )
                return None

            return A11yResult(
                bbox=(x, y, w, h),
                name=element.Name or "",
                role=element.ControlTypeName or "unknown",
            )
        except Exception as e:
            logger.debug("Failed to extract bbox from UIA element: %s", e)
            return None
