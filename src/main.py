"""AI Navigator — Entry point and main guidance loop.

Integrates all components: Qt UI, asyncio for API calls, multiprocessing
for OCR/screen-diff, and the core guidance engine.

Architecture:
- Main process: Qt event loop + asyncio (via threading)
- OCR worker: separate process (GIL mitigation)
- Screen diff worker: separate process (GIL mitigation)
- A11y queries: main process (< 5ms, I/O not CPU)
"""

import asyncio
import ctypes
import logging
import sys
import threading
import time
from collections import deque
from typing import Optional

from PySide6.QtCore import QObject, QTimer, Signal, Slot
from PySide6.QtWidgets import QApplication

from src.ai.api_router import APIRouter, BudgetExceededError
from src.ai.tool_schemas import NavigateStepResponse
from src.config import Config, get_config, setup_logging
from src.core.correction import CorrectionHandler
from src.core.cost_tracker import CostTracker
from src.core.session import Session, SessionManager
from src.core.step_sequencer import StepSequencer
from src.input.chat_input import ChatInputHandler
from src.input.voice_input import VoiceInput
from src.input.screen_capture import (
    capture_for_guidance,
    capture_screenshot_raw,
    image_to_bytes,
)
from src.input.screen_monitor import ScreenChangeEvent, ScreenMonitor
from src.locator.element_locator import ElementLocator
from src.output.clipboard import copy_to_clipboard
from src.output.overlay import OverlayWindow
from src.output.tts import TTSEngine
from src.ui.panel_window import ConsolidatedPanel

logger = logging.getLogger(__name__)


class GuidanceEngine(QObject):
    """Core guidance engine that orchestrates the navigation loop.

    Runs async operations in a background thread and communicates
    with the Qt UI via signals.
    """

    # Signals for thread-safe UI updates
    response_ready = Signal(object)  # NavigateStepResponse
    error_occurred = Signal(str)
    processing_started = Signal()
    processing_finished = Signal()
    streaming_chunk = Signal(str)    # Partial instruction text during streaming
    voice_transcript = Signal(str)   # Recognized speech from background voice thread

    def __init__(self, config: Config) -> None:
        super().__init__()

        self._config = config

        # Core components
        self._cost_tracker = CostTracker(
            daily_cap=config.daily_token_cap,
            monthly_cap=config.monthly_token_cap,
            safety_margin=config.cost_safety_margin,
            storage_path=config.token_usage_file,
        )
        self._session_manager = SessionManager(session_dir=config.session_dir)
        self._api_router = APIRouter(config=config, cost_tracker=self._cost_tracker)
        self._step_sequencer = StepSequencer()
        self._correction_handler = CorrectionHandler(api_router=self._api_router)
        self._chat_input = ChatInputHandler()
        self._element_locator = ElementLocator(
            enable_a11y=config.enable_a11y,
            enable_ocr=config.enable_ocr,
            ocr_lang=config.ocr_lang,
            ocr_confidence_threshold=config.ocr_confidence_threshold,
            a11y_timeout_ms=config.a11y_timeout_ms,
        )
        self._screen_monitor = ScreenMonitor(
            diff_threshold=config.diff_threshold,
            phash_threshold=config.phash_threshold,
            idle_timeout_sec=config.idle_timeout_sec,
            diff_fps=config.diff_fps,
            thumbnail_size=(config.diff_thumbnail_width, config.diff_thumbnail_height),
        )

        # State
        self._is_processing = False
        self._active = False
        self._pending_messages: deque[str] = deque()  # Queued user messages during processing
        self._last_api_call_time: float = 0.0  # Debounce re-queries from screen changes
        self._screen_change_requery_cooldown_sec: float = 2.0
        self._last_screenshot_size: tuple[int, int] = (0, 0)  # (width, height) of last captured image
        self._next_turn_full_screen: bool = False  # Set when AI requests full desktop screenshot
        self._had_screen_change_since_step: bool = False  # True once a real diff event arrives after step loaded
        # Callback to get the AI Navigator window geometry (set by Application after init)
        # Returns a string like "AI Navigator window: top-left corner, 600x750px" or None
        self._get_window_context: Optional[callable] = None

        # Async event loop in background thread
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None

    def start(self) -> None:
        """Start background workers and async event loop."""
        self._element_locator.start()
        self._screen_monitor.start()
        self._active = True

        # Start asyncio event loop in background thread
        self._thread = threading.Thread(target=self._run_async_loop, daemon=True, name="async-loop")
        self._thread.start()

        logger.info("Guidance engine started")

    def stop(self) -> None:
        """Stop all workers and clean up."""
        self._active = False
        self._screen_monitor.stop()
        self._element_locator.stop()

        if self._loop:
            self._loop.call_soon_threadsafe(self._loop.stop)
        if self._thread:
            self._thread.join(timeout=5)

        logger.info("Guidance engine stopped")

    def _run_async_loop(self) -> None:
        """Run the asyncio event loop in a background thread."""
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        self._loop.run_forever()

    def _run_async(self, coro) -> None:
        """Schedule a coroutine on the background async loop."""
        if self._loop and self._loop.is_running():
            asyncio.run_coroutine_threadsafe(coro, self._loop)

    # --- Public API (called from Qt main thread) ---

    def handle_user_message(self, text: str) -> None:
        """Handle a new user message.

        If already processing, the message is queued and sent automatically
        once the current response finishes (so the input box can stay enabled).
        """
        if self._is_processing:
            self._pending_messages.append(text)
            logger.debug("Message queued (processing in progress): %s", text[:50])
            return

        session = self._session_manager.current_session
        if session is None:
            session = self._session_manager.create_session(task_description=text)
            session.add_turn(role="user", content=text)
            self._run_async(self._send_initial_request(session, text))
        else:
            session.add_turn(role="user", content=text)
            self._run_async(self._send_followup_request(session, text))

    def handle_screen_change(self, event: ScreenChangeEvent) -> None:
        """Handle a screen change event (from screen monitor).

        Three cases handled here:
        1. Checkpoint step + LARGE change (>30% pixels) → auto-advance.
           Full page navigation / new dialog reliably means the user completed the action.
        2. Mid-sequence non-checkpoint step → auto-advance on any detected change.
        3. Sequence complete with active session → re-query AI (debounced).

        Small screen changes at a checkpoint (radio ticks, focus rings, tooltips) are
        still ignored — those still require the → Next button.
        """
        if self._is_processing or not self._active:
            return

        session = self._session_manager.current_session
        if session is None:
            return

        # Track real screen changes (diff events with nonzero change) so the idle
        # branch below can tell whether the user actually did something.
        if event.source == "diff" and event.change_pct > 0:
            self._had_screen_change_since_step = True

        if self._step_sequencer.is_at_checkpoint:
            # Checkpoint step: auto-complete when the feature is enabled AND either:
            #   a) a large pixel change occurred (page navigation, new dialog), OR
            #   b) the idle timer fired AND a real screen change was seen since the step
            #      loaded (small interaction settled) — user completed a small action.
            # Idle without any prior change = user hasn't touched anything → do NOT advance.
            # Small transient changes (radio ticks, focus rings) still require → Next.
            if not self._config.checkpoint_auto_advance:
                return
            is_large = event.change_pct > self._config.checkpoint_auto_advance_threshold
            is_idle  = event.source == "idle" and self._had_screen_change_since_step
            if not is_large and not is_idle:
                return
            # Large change OR idle-after-interaction → fall through and advance below.

        if not self._step_sequencer.is_complete:
            # Mid-sequence non-checkpoint step: screen changed → auto-advance.
            # advance() is synchronous so state changes before the next event fires.
            next_step = self._step_sequencer.advance()
            if next_step:
                self.response_ready.emit(self._step_sequencer)
            elif self._step_sequencer.is_complete:
                # Last step just consumed — re-query AI
                self._is_processing = True
                self.processing_started.emit()
                self._run_async(self._send_screen_change_followup(session))

        elif session.current_state_summary:
            # Sequence complete, task in progress — re-query if enough time has passed
            now = time.monotonic()
            if now - self._last_api_call_time >= self._screen_change_requery_cooldown_sec:
                self._is_processing = True
                self.processing_started.emit()
                self._run_async(self._send_screen_change_followup(session))

    def handle_correction(self) -> None:
        """Handle correction hotkey press."""
        session = self._session_manager.current_session
        if session is None:
            return
        self._run_async(self._handle_correction(session))

    def handle_next_step(self) -> None:
        """Handle next-step button / hotkey press.

        Advances past the current step (checkpoint or not).  If the sequence is
        now complete, re-queries the AI with completion context so it knows to
        move on rather than repeat the same instruction.
        """
        if self._is_processing:
            return

        # Capture the instruction BEFORE advancing so we can tell the AI what was done.
        completed_instruction = (
            self._step_sequencer.current_step.instruction
            if self._step_sequencer.current_step else ""
        )

        next_step = self._step_sequencer.advance()
        if next_step:
            # More steps remain — show next one immediately.
            self.response_ready.emit(self._step_sequencer)
            return

        # Sequence complete — re-query AI so it knows what was just done and
        # can provide the next instruction.
        session = self._session_manager.current_session
        if session is None:
            return

        completion_prefix = (
            f"[User completed: '{completed_instruction}']\n\n"
            if completed_instruction else ""
        )
        self._is_processing = True
        self.processing_started.emit()
        self._run_async(
            self._send_followup_request(
                session,
                f"{completion_prefix}The user has completed the previous step. "
                "Here is the updated screen. What is the next step?",
                use_fast_model=False,  # User explicitly triggered — use full model
            )
        )

    def toggle_pause(self) -> bool:
        """Toggle screen monitoring pause."""
        return self._screen_monitor.toggle_pause()

    # --- Async operations (run in background thread) ---

    async def _send_initial_request(self, session: Session, task_description: str) -> None:
        """Send the first guidance request for a new task."""
        if not self._is_processing:
            self._is_processing = True
            self.processing_started.emit()

        try:
            force_full = self._next_turn_full_screen
            self._next_turn_full_screen = False
            screenshot_b64, img = capture_for_guidance(force_full=force_full)
            self._last_screenshot_size = (img.width, img.height)
            self._element_locator.start_ocr_precache(image_to_bytes(img))

            response = await self._api_router.send_initial_request(
                task_description=task_description,
                screenshot_b64=screenshot_b64,
                session=session,
                on_text_chunk=self.streaming_chunk.emit,
            )

            self._last_api_call_time = time.monotonic()
            # Finish streaming UI before adding final message text
            self._is_processing = False
            self.processing_finished.emit()
            self._process_response(session, response)

        except BudgetExceededError as e:
            self._is_processing = False
            self.processing_finished.emit()
            self.error_occurred.emit(str(e))
        except Exception as e:
            logger.error("Initial request failed: %s", e)
            self._is_processing = False
            self.processing_finished.emit()
            self.error_occurred.emit(f"API request failed: {e}")
        finally:
            self._flush_pending_messages(session)

    async def _send_followup_request(self, session: Session, user_text: str, use_fast_model: bool = False) -> None:
        """Send a follow-up guidance request."""
        if not self._is_processing:
            self._is_processing = True
            self.processing_started.emit()

        try:
            force_full = self._next_turn_full_screen
            self._next_turn_full_screen = False
            screenshot_b64, img = capture_for_guidance(force_full=force_full)
            self._last_screenshot_size = (img.width, img.height)
            self._element_locator.start_ocr_precache(image_to_bytes(img))

            state_summary = None
            if session.current_state_summary:
                state_summary = session.current_state_summary.summary_text

            # Append AI Navigator window position so Claude knows where the app sits
            window_ctx = self._get_window_context() if self._get_window_context else None
            if window_ctx:
                state_summary = f"{state_summary}\n{window_ctx}" if state_summary else window_ctx

            response = await self._api_router.send_guidance_request(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                session=session,
                on_text_chunk=self.streaming_chunk.emit,
                use_fast_model=use_fast_model,
            )

            self._last_api_call_time = time.monotonic()
            # Finish streaming UI before adding final message text
            self._is_processing = False
            self.processing_finished.emit()
            self._process_response(session, response)

        except BudgetExceededError as e:
            self._is_processing = False
            self.processing_finished.emit()
            self.error_occurred.emit(str(e))
        except Exception as e:
            logger.error("Follow-up request failed: %s", e)
            self._is_processing = False
            self.processing_finished.emit()
            self.error_occurred.emit(f"API request failed: {e}")
        finally:
            self._flush_pending_messages(session)

    async def _send_screen_change_followup(self, session: Session) -> None:
        """Re-query the AI after a screen change when no active steps remain.

        Uses the fast model (Haiku) — screen-change re-queries are frequent and
        the task context is already established, so a cheaper model is sufficient.
        """
        await self._send_followup_request(
            session,
            "The screen changed. Here is the current screen. What is the next step?",
            use_fast_model=True,
        )

    async def _handle_checkpoint_completed(self, session: Session) -> None:
        """Handle when user completes a checkpoint step."""
        # Capture the completed instruction BEFORE advancing so we can record it.
        completed_instruction = (
            self._step_sequencer.current_step.instruction
            if self._step_sequencer.current_step
            else ""
        )

        # Advance past the checkpoint
        next_step = self._step_sequencer.advance()

        if next_step and not self._step_sequencer.is_at_checkpoint:
            # More non-checkpoint steps — show them without an API call.
            # Reset _is_processing since no API call is being made.
            self._is_processing = False
            self.processing_finished.emit()
            self.response_ready.emit(self._step_sequencer)
        elif self._step_sequencer.is_complete:
            # Sequence done — record completion in session so AI doesn't repeat the
            # last instruction, then query AI for the next steps.
            if completed_instruction:
                session.add_turn(
                    role="user",
                    content=f"[Completed: {completed_instruction}]",
                )
            await self._send_followup_request(
                session,
                "The user just completed the previous step. Here is the updated screen. What is the next step?",
                use_fast_model=True,
            )
        else:
            # Next step is also a checkpoint — show it without an API call.
            self._is_processing = False
            self.processing_finished.emit()
            self.response_ready.emit(self._step_sequencer)

    def _flush_pending_messages(self, session: Optional[Session]) -> None:
        """Send the oldest queued message if one exists (called after processing finishes)."""
        if self._pending_messages and session is not None and not self._is_processing:
            text = self._pending_messages.popleft()
            logger.debug("Flushing queued message: %s", text[:50])
            session.add_turn(role="user", content=text)
            self._run_async(self._send_followup_request(session, text))

    async def _handle_correction(self, session: Session) -> None:
        """Handle a correction request."""
        self.processing_started.emit()
        self._is_processing = True

        try:
            response = await self._correction_handler.handle_correction(session)
            self._is_processing = False
            self.processing_finished.emit()
            if response:
                self._process_response(session, response)

        except Exception as e:
            logger.error("Correction failed: %s", e)
            self._is_processing = False
            self.processing_finished.emit()
            self.error_occurred.emit(f"Correction failed: {e}")

    def _process_response(self, session: Session, response: NavigateStepResponse) -> None:
        """Process an AI response: update state, load steps, emit signal."""
        # If the AI needs to see the full desktop next turn, set the flag.
        if response.request_full_screen:
            self._next_turn_full_screen = True
            logger.debug("AI requested full-screen screenshot for next turn")

        # Update session state
        session.update_state(response.state_summary)
        if response.steps:
            instruction_text = response.steps[0].instruction
            session.add_turn(role="assistant", content=instruction_text)

        # Load steps into sequencer; reset change flag so idle won't fire until user acts
        self._had_screen_change_since_step = False
        self._step_sequencer.load_steps(response.steps)

        # Save session
        try:
            self._session_manager.save_session(session)
        except Exception as e:
            logger.warning("Failed to auto-save session: %s", e)

        # Emit to UI thread
        self.response_ready.emit(self._step_sequencer)

    @property
    def session_manager(self) -> SessionManager:
        return self._session_manager

    @property
    def cost_tracker(self) -> CostTracker:
        return self._cost_tracker

    @property
    def screen_monitor(self) -> ScreenMonitor:
        return self._screen_monitor

    @property
    def step_sequencer(self) -> StepSequencer:
        return self._step_sequencer

    @property
    def element_locator(self) -> ElementLocator:
        return self._element_locator

    @property
    def last_screenshot_size(self) -> tuple[int, int]:
        """(width, height) of the most recently captured screenshot (post-resize)."""
        return self._last_screenshot_size

    @last_screenshot_size.setter
    def last_screenshot_size(self, value: tuple[int, int]) -> None:
        self._last_screenshot_size = value


class Application:
    """Main application class that wires together all components."""

    def __init__(self) -> None:
        self._config = get_config()
        setup_logging(self._config)

        # Qt Application
        self._app = QApplication(sys.argv)
        self._app.setApplicationName("AI Navigator")
        self._app.setStyle("Fusion")

        # Apply dark theme
        self._apply_dark_theme()

        # Core engine
        self._engine = GuidanceEngine(self._config)

        # UI components
        self._panel = ConsolidatedPanel()
        self._overlay = OverlayWindow()

        # TTS engine (optional)
        self._tts = TTSEngine(
            rate=self._config.tts_rate,
            volume=self._config.tts_volume,
        )
        if self._config.enable_tts and self._tts.is_available:
            self._tts.start()
            logger.info("TTS enabled")

        # Voice input (optional) — transcript callback emits a Qt signal for thread safety
        self._voice_input = VoiceInput(
            on_transcript=self._engine.voice_transcript.emit,
            language="en-US",
        )
        if self._config.enable_voice_input and self._voice_input.is_available:
            self._voice_input.start()
            self._panel.set_voice_enabled(True)
            logger.info("Voice input enabled")
            # Poll is_listening at 200ms to sync button state after timeouts
            self._voice_timer = QTimer()
            self._voice_timer.setInterval(200)
            self._voice_timer.timeout.connect(self._sync_listening_state)
            self._voice_timer.start()
        else:
            self._voice_timer = None

        # Configure overlay from config
        self._overlay.set_colors(self._config.overlay_color, self._config.overlay_thickness)
        self._overlay.set_subtitle_style(
            self._config.subtitle_font_size, self._config.subtitle_bg_opacity,
        )

        # Give engine a way to read the AI Navigator panel position
        self._engine._get_window_context = self._get_window_context

        # Connect signals
        self._connect_signals()

        # Global hotkeys
        self._setup_hotkeys()

        # Screen monitor polling timer
        self._monitor_timer = QTimer()
        self._monitor_timer.setInterval(50)  # 50ms = 20Hz polling
        self._monitor_timer.timeout.connect(self._engine.screen_monitor.poll)

    def _setup_hotkeys(self) -> None:
        """Register global hotkeys via Win32 RegisterHotKey + Qt native event filter.

        RegisterHotKey is the most reliable Windows global hotkey API — it posts
        WM_HOTKEY to the thread message queue and is not affected by GIL contention
        or hook timeouts that plague SetWindowsHookEx-based libraries.
        """
        import ctypes
        import ctypes.wintypes
        from PySide6.QtCore import QAbstractNativeEventFilter

        MOD_CONTROL  = 0x0002
        MOD_SHIFT    = 0x0004
        MOD_NOREPEAT = 0x4000

        # Virtual key codes for single-character keys and special keys
        VK_EXTRA = {"space": 0x20, "f1": 0x70, "f2": 0x71, "f3": 0x72,
                    "f4": 0x73, "f5": 0x74, "f12": 0x7B}

        def parse(hotkey_str: str) -> tuple[int, int]:
            """Parse 'ctrl+shift+x' → (modifiers, vk_code)."""
            mods = MOD_NOREPEAT
            vk = 0
            for part in hotkey_str.lower().split("+"):
                part = part.strip()
                if part == "ctrl":    mods |= MOD_CONTROL
                elif part == "shift": mods |= MOD_SHIFT
                elif part in VK_EXTRA: vk = VK_EXTRA[part]
                elif len(part) == 1:  vk = ord(part.upper())
            return mods, vk

        definitions = [
            (1, self._config.correction_hotkey,      self._on_correction),
            (2, self._config.pause_hotkey,           self._on_pause_toggle),
            (3, self._config.next_step_hotkey,       self._on_next_step),
            (4, self._config.floating_window_hotkey, self._panel.toggle_visibility),
        ]

        registered: dict[int, object] = {}
        for hid, hotkey_str, callback in definitions:
            mods, vk = parse(hotkey_str)
            if vk and ctypes.windll.user32.RegisterHotKey(None, hid, mods, vk):
                registered[hid] = callback
            else:
                logger.warning("Failed to register hotkey %d: %s (vk=0x%02X)", hid, hotkey_str, vk)

        WM_HOTKEY = 0x0312

        class _HotkeyFilter(QAbstractNativeEventFilter):
            def nativeEventFilter(self_, eventType, message):  # noqa: N805
                if eventType == b"windows_generic_MSG":
                    msg = ctypes.wintypes.MSG.from_address(int(message))
                    if msg.message == WM_HOTKEY:
                        cb = registered.get(msg.wParam)
                        if cb:
                            cb()
                return False, 0

        self._hotkey_filter = _HotkeyFilter()
        self._app.installNativeEventFilter(self._hotkey_filter)
        self._registered_hotkey_ids = list(registered.keys())

        logger.info(
            "Hotkeys registered (Win32): correction=%s, pause=%s, next=%s, float=%s",
            self._config.correction_hotkey, self._config.pause_hotkey,
            self._config.next_step_hotkey, self._config.floating_window_hotkey,
        )

    def _get_window_context(self) -> Optional[str]:
        """Return the AI Navigator panel position as a context string for the AI."""
        geo = self._panel.geometry()
        return (
            f"[AI Navigator window position: x={geo.x()}, y={geo.y()}, "
            f"width={geo.width()}, height={geo.height()}. "
            f"If this window covers important content, tell the user to minimize or move it — NOT close it.]"
        )

    def _build_monitor_map(self) -> None:
        """Build a mapping from physical mss monitors to Qt logical screens.

        Both A11y (UIA BoundingRectangle) and OCR (mss pixels) return physical
        pixel coordinates in the virtual desktop coordinate space. Qt overlay
        uses logical pixels. Conversion is per-screen because each monitor can
        have a different DPR (e.g. 2.0 on a HiDPI screen, 1.0 on a 1080p screen).

        Monitors are matched by sorting both mss and Qt screens left-to-right,
        which correctly handles typical multi-monitor horizontal arrangements.
        """
        import mss as _mss
        with _mss.mss() as sct:
            # monitors[0] is the virtual desktop aggregate; 1..N are physical monitors.
            # monitors[0].left/top is the virtual desktop origin, which can be negative
            # when a monitor is positioned to the left or above the primary.
            # mss screenshots always start at pixel (0,0) = virtual (left, top), so
            # OCR coordinates (always ≥ 0) must be offset by this origin to obtain
            # virtual screen coordinates before the physical→logical conversion.
            self._virtual_origin = (sct.monitors[0]["left"], sct.monitors[0]["top"])
            self._vd_width = sct.monitors[0]["width"]
            self._vd_height = sct.monitors[0]["height"]
            mss_monitors = sorted(sct.monitors[1:], key=lambda m: m["left"])
        qt_screens = sorted(self._app.screens(), key=lambda s: s.geometry().x())
        self._monitor_map = list(zip(mss_monitors, qt_screens))
        logger.info("Virtual desktop origin (mss): %s", self._virtual_origin)
        for mon, screen in self._monitor_map:
            logger.info(
                "Monitor map: mss physical left=%d top=%d w=%d h=%d  ↔  "
                "Qt logical x=%d y=%d w=%d h=%d DPR=%.2f",
                mon["left"], mon["top"], mon["width"], mon["height"],
                screen.geometry().x(), screen.geometry().y(),
                screen.geometry().width(), screen.geometry().height(),
                screen.devicePixelRatio(),
            )

    def _scale_to_logical(self, bbox: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
        """Convert a physical-pixel bbox to Qt logical pixels.

        Finds which physical monitor contains the bbox centre, then divides by
        that screen's DPR and offsets by that screen's logical origin.
        Falls back to the primary screen's DPR if no match found.
        """
        x, y, w, h = bbox
        cx, cy = x + w // 2, y + h // 2
        for mon, screen in self._monitor_map:
            ml, mt, mw, mh = mon["left"], mon["top"], mon["width"], mon["height"]
            if ml <= cx < ml + mw and mt <= cy < mt + mh:
                dpr = screen.devicePixelRatio()
                geom = screen.geometry()
                return (
                    int(geom.x() + (x - ml) / dpr),
                    int(geom.y() + (y - mt) / dpr),
                    int(w / dpr),
                    int(h / dpr),
                )
        # Fallback: primary screen DPR, no offset correction
        dpr = self._app.primaryScreen().devicePixelRatio()
        return (int(x / dpr), int(y / dpr), int(w / dpr), int(h / dpr))

    def _apply_dark_theme(self) -> None:
        """Apply a dark color palette."""
        from PySide6.QtGui import QColor, QPalette

        palette = QPalette()
        palette.setColor(QPalette.ColorRole.Window, QColor(30, 30, 30))
        palette.setColor(QPalette.ColorRole.WindowText, QColor(212, 212, 212))
        palette.setColor(QPalette.ColorRole.Base, QColor(25, 25, 25))
        palette.setColor(QPalette.ColorRole.AlternateBase, QColor(45, 45, 45))
        palette.setColor(QPalette.ColorRole.Text, QColor(212, 212, 212))
        palette.setColor(QPalette.ColorRole.Button, QColor(45, 45, 45))
        palette.setColor(QPalette.ColorRole.ButtonText, QColor(212, 212, 212))
        palette.setColor(QPalette.ColorRole.Highlight, QColor(255, 107, 53))
        palette.setColor(QPalette.ColorRole.HighlightedText, QColor(255, 255, 255))
        self._app.setPalette(palette)

    def _connect_signals(self) -> None:
        """Wire up all signal connections between components."""
        # User input → engine
        self._panel.message_submitted.connect(self._on_user_message)

        # Engine → UI
        self._engine.response_ready.connect(self._on_response_ready)
        self._engine.error_occurred.connect(self._on_error)
        self._engine.processing_started.connect(self._on_processing_started)
        self._engine.processing_finished.connect(self._on_processing_finished)
        self._engine.streaming_chunk.connect(self._panel.append_streaming_chunk)

        # Screen monitor → engine
        self._engine.screen_monitor.on_change(self._engine.handle_screen_change)

        # Panel actions
        self._panel.correction_requested.connect(self._on_correction)
        self._panel.pause_toggled.connect(self._on_pause_toggle)
        self._panel.next_step_requested.connect(self._on_next_step)
        self._panel.mic_pressed.connect(self._on_mic_pressed)
        self._panel.new_session_requested.connect(self._on_new_session)
        self._panel.save_session_requested.connect(self._on_save_session)

        # Voice input → engine (thread-safe via signal)
        self._engine.voice_transcript.connect(self._on_voice_transcript)

    @Slot(str)
    def _on_user_message(self, text: str) -> None:
        """Handle user message from any input source."""
        self._panel.add_message("user", text)
        self._engine.handle_user_message(text)

    @Slot()
    def _on_processing_started(self) -> None:
        self._panel.set_processing(True)
        self._streaming_active = True
        self._panel.begin_streaming_message()

    @Slot()
    def _on_processing_finished(self) -> None:
        self._panel.set_processing(False)
        if getattr(self, "_streaming_active", False):
            self._panel.end_streaming_message()
            self._streaming_active = False

    @Slot(object)
    def _on_response_ready(self, sequencer: StepSequencer) -> None:
        """Handle AI response — show overlay and update chat."""
        step = sequencer.current_step
        if step is None:
            return

        # Always write the final instruction via add_message — streaming is
        # visual-only feedback and is removed by end_streaming_message before this.
        self._panel.add_message("assistant", step.instruction)

        # Speak the instruction if TTS is enabled
        if self._config.enable_tts and self._tts.is_available:
            self._tts.speak(step.instruction)

        # Show progress
        progress = sequencer.get_progress()
        if progress:
            self._panel.show_status(progress)

        # Copy to clipboard if needed
        if step.clipboard:
            copy_to_clipboard(step.clipboard)
            self._panel.add_message(
                "system", f"Copied to clipboard: {step.clipboard}"
            )

        # Refresh OCR with a live screenshot.  Multi-step sequences can outlive
        # the pre-call OCR cache (which was taken before the API call, possibly
        # 2-3 screen changes ago), causing "element not found" on later steps.
        # Windows OCR finishes in ~10ms so results are usually ready by the time
        # processEvents() returns below.
        try:
            fresh_img = capture_screenshot_raw()
            self._engine.last_screenshot_size = (fresh_img.width, fresh_img.height)
            self._engine.element_locator.start_ocr_precache(image_to_bytes(fresh_img))
        except Exception:
            pass  # non-fatal; stale cache is better than crashing

        # Flush pending Qt repaints AND give Windows OCR a moment to finish.
        # Calling processEvents() twice with a yield gives ~10-20ms gap which
        # is enough for the OCR worker to drain its result queue.
        QApplication.processEvents()
        QApplication.processEvents()

        # Locate element and show overlay.
        # Enforce the 1–5 word limit on target_text regardless of what the model
        # returned — long strings (e.g. full placeholder text) never match.
        # Also skip locate for very short strings (< 3 chars): single/double-char
        # targets ("I", "A") reliably match garbage off-screen UIA elements.
        if step.target_text:
            words = step.target_text.split()
            target_text = " ".join(words[:5]) if len(words) > 5 else step.target_text
            # Skip element location for very short strings (< 3 chars).
            # Single/double-char targets ("I", "A") reliably match garbage
            # off-screen UIA elements; fall back to subtitle immediately.
            if len(target_text.strip()) < 3:
                logger.debug(
                    "target_text '%s' too short for reliable location — subtitle fallback",
                    target_text,
                )
                self._overlay.show_subtitle(step.instruction)
            else:
                result = self._engine.element_locator.locate(
                    target_text=target_text,
                    target_role=step.target_role.value if step.target_role else None,
                    target_region=step.target_region.value if step.target_region else None,
                )

                if result.bbox:
                    # Coordinate spaces:
                    # - A11y (UIA BoundingRectangle): virtual screen coords (can be negative).
                    # - OCR (mss image): image-relative coords in the downscaled screenshot.
                    #   The screenshot is captured at full virtual-desktop resolution then
                    #   downscaled (thumbnail) before OCR — so OCR coords are in image space,
                    #   not virtual desktop space. Steps to convert:
                    #   1. Scale up from image pixels to virtual desktop pixels.
                    #   2. Add virtual origin offset to get virtual screen coordinates.
                    if result.method == "ocr":
                        ox, oy = self._virtual_origin
                        x, y, w, h = result.bbox
                        ss_w, ss_h = self._engine.last_screenshot_size
                        if ss_w > 0 and self._vd_width > 0:
                            sx = self._vd_width / ss_w
                            sy = self._vd_height / ss_h
                        else:
                            sx, sy = 1.0, 1.0
                        virt_bbox = (
                            int(x * sx) + ox,
                            int(y * sy) + oy,
                            int(w * sx),
                            int(h * sy),
                        )
                    else:
                        virt_bbox = result.bbox
                    scaled = self._scale_to_logical(virt_bbox)
                    logger.info(
                        "Overlay: method=%s image=%s virtual=%s → logical=%s",
                        result.method, result.bbox, virt_bbox, scaled,
                    )
                    self._overlay.show_overlay(
                        bbox=scaled,
                        overlay_type=step.overlay_type.value,
                        instruction=step.instruction,
                    )
                else:
                    self._overlay.show_subtitle(step.instruction)
        else:
            self._overlay.show_subtitle(step.instruction)

        # Update token display
        session = self._engine.session_manager.current_session
        if session:
            self._panel.update_token_display(session.total_tokens)

    @Slot(str)
    def _on_error(self, message: str) -> None:
        """Handle errors from the engine."""
        self._panel.add_message("system", f"Error: {message}")
        logger.error("Engine error: %s", message)

    @Slot()
    def _on_correction(self) -> None:
        """Handle correction request."""
        self._tts.stop()
        self._panel.add_message("system", "Correction requested — re-analyzing...")
        self._overlay.clear()
        self._engine.handle_correction()

    @Slot()
    def _on_pause_toggle(self) -> None:
        """Handle pause toggle."""
        paused = self._engine.toggle_pause()
        self._panel.set_paused(paused)
        if paused:
            self._overlay.clear()

    @Slot()
    def _on_next_step(self) -> None:
        """Handle next step request."""
        self._overlay.clear()
        self._engine.handle_next_step()

    @Slot()
    def _on_mic_pressed(self) -> None:
        """Trigger a push-to-talk listen cycle."""
        if self._voice_input.is_listening:
            return  # Already recording
        self._panel.set_listening(True)
        self._voice_input.listen()

    @Slot(str)
    def _on_voice_transcript(self, text: str) -> None:
        """Handle recognized speech — route it as a user message."""
        self._panel.set_listening(False)
        self._on_user_message(text)

    @Slot()
    def _sync_listening_state(self) -> None:
        """Poll voice input listening state to sync button after timeouts."""
        self._panel.set_listening(self._voice_input.is_listening)

    @Slot()
    def _on_new_session(self) -> None:
        """Handle new session request."""
        self._panel.clear_chat()
        self._overlay.clear()
        self._engine.step_sequencer.reset()
        self._panel.add_message("system", "New session started. What would you like help with?")

    @Slot()
    def _on_save_session(self) -> None:
        """Handle save session request."""
        session = self._engine.session_manager.current_session
        if session:
            path = self._engine.session_manager.save_session(session)
            self._panel.add_message("system", f"Session saved: {path.name}")
        else:
            self._panel.add_message("system", "No active session to save")

    def run(self) -> int:
        """Start the application and enter the event loop."""
        logger.info("AI Navigator starting...")

        # Check API availability
        router = self._engine._api_router
        if not router.is_available:
            provider = self._config.api_provider
            if provider == "gemini":
                hint = "Set GEMINI_API_KEY in .env. Free key at https://aistudio.google.com/apikey"
            elif provider == "ollama":
                hint = (
                    f"Ollama server not reachable at {self._config.ollama_base_url}. "
                    f"Start it with: ollama serve\n"
                    f"Pull a vision model: ollama pull {self._config.ollama_model}"
                )
            else:
                hint = "Set ANTHROPIC_API_KEY in .env."
            self._panel.add_message("system", f"Warning: {hint}")
        else:
            self._panel.add_message(
                "system",
                f"AI Navigator ready — using {router.provider_name}\n"
                "Type a task description to get started.\n"
                "Example: 'Help me buy a USB-C cable on Amazon'"
            )

        # Tell the element locator the full virtual desktop size so OCR region
        # filtering works correctly across all monitors (not just primary).
        virtual = self._app.primaryScreen().virtualGeometry()
        self._engine.element_locator.set_screen_size(virtual.width(), virtual.height())
        logger.debug("Virtual desktop size: %dx%d", virtual.width(), virtual.height())

        # Build monitor map for physical→logical pixel conversion
        self._build_monitor_map()

        # Start engine
        self._engine.start()

        # Start screen monitor polling
        self._monitor_timer.start()

        # Show panel (starts expanded at bottom-right)
        self._panel.show()

        # Run Qt event loop
        try:
            exit_code = self._app.exec()
        finally:
            self._monitor_timer.stop()
            if self._voice_timer:
                self._voice_timer.stop()
            try:
                import ctypes
                for hid in getattr(self, "_registered_hotkey_ids", []):
                    ctypes.windll.user32.UnregisterHotKey(None, hid)
            except Exception:
                pass
            self._engine.stop()
            self._tts.shutdown()
            self._voice_input.shutdown()

        return exit_code


def main() -> None:
    """Entry point for AI Navigator."""
    # On Windows, allow the app to show its own taskbar icon
    if sys.platform == "win32":
        try:
            ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID("ai-navigator.v0.1")
        except Exception:
            pass

    app = Application()
    sys.exit(app.run())


if __name__ == "__main__":
    main()
