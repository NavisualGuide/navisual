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
from src.input.screen_capture import (
    capture_screenshot_b64,
    image_to_bytes,
)
from src.input.screen_monitor import ScreenChangeEvent, ScreenMonitor
from src.locator.element_locator import ElementLocator
from src.output.clipboard import copy_to_clipboard
from src.output.overlay import OverlayWindow
from src.ui.floating_window import FloatingWindow
from src.ui.main_window import MainWindow

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
        self._screen_change_requery_cooldown_sec: float = 5.0
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

        Three cases:
        1. At a checkpoint step → user completed it, advance/re-query.
        2. Mid-sequence (non-checkpoint) → screen changed, auto-advance to next step.
        3. Sequence complete with active session → re-query AI (debounced).
        """
        if self._is_processing or not self._active:
            return

        session = self._session_manager.current_session
        if session is None:
            return

        if self._step_sequencer.is_at_checkpoint:
            # User completed a checkpoint step.
            # Set _is_processing synchronously here — before _run_async — so
            # subsequent screen-change events (firing 10ms later at 10fps) see
            # the flag and bail immediately instead of scheduling duplicate calls.
            self._is_processing = True
            self.processing_started.emit()
            self._run_async(self._handle_checkpoint_completed(session))

        elif not self._step_sequencer.is_complete:
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
        """Handle next-step hotkey press."""
        step = self._step_sequencer.advance()
        if step:
            self.response_ready.emit(self._step_sequencer)

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
            screenshot_b64, img = capture_screenshot_b64()
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

    async def _send_followup_request(self, session: Session, user_text: str) -> None:
        """Send a follow-up guidance request."""
        if not self._is_processing:
            self._is_processing = True
            self.processing_started.emit()

        try:
            screenshot_b64, img = capture_screenshot_b64()
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
        """Re-query the AI after a screen change when no active steps remain."""
        await self._send_followup_request(
            session,
            "The screen changed. Here is the current screen. What is the next step?",
        )

    async def _handle_checkpoint_completed(self, session: Session) -> None:
        """Handle when user completes a checkpoint step."""
        # Advance past the checkpoint
        next_step = self._step_sequencer.advance()

        if next_step and not self._step_sequencer.is_at_checkpoint:
            # More non-checkpoint steps — show them without an API call.
            # Reset _is_processing since no API call is being made.
            self._is_processing = False
            self.processing_finished.emit()
            self.response_ready.emit(self._step_sequencer)
        elif self._step_sequencer.is_complete:
            # Sequence done — query AI for next steps
            await self._send_followup_request(
                session,
                "The user completed the previous step. Here is the current screen.",
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
        # Update session state
        session.update_state(response.state_summary)
        if response.steps:
            instruction_text = response.steps[0].instruction
            session.add_turn(role="assistant", content=instruction_text)

        # Load steps into sequencer
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
        self._main_window = MainWindow()
        self._overlay = OverlayWindow()
        self._floating_window = FloatingWindow()

        # Configure overlay from config
        self._overlay.set_colors(self._config.overlay_color, self._config.overlay_thickness)
        self._overlay.set_subtitle_style(
            self._config.subtitle_font_size, self._config.subtitle_bg_opacity,
        )

        # Give engine a way to read the AI Navigator window position
        self._engine._get_window_context = self._get_window_context

        # Connect signals
        self._connect_signals()

        # Screen monitor polling timer
        self._monitor_timer = QTimer()
        self._monitor_timer.setInterval(50)  # 50ms = 20Hz polling
        self._monitor_timer.timeout.connect(self._engine.screen_monitor.poll)

    def _get_window_context(self) -> Optional[str]:
        """Return the AI Navigator window position as a context string for the AI.

        This lets Claude know where the app window is so it can suggest
        'minimize it' rather than 'close it' when the window occludes the screen.
        """
        geo = self._main_window.geometry()
        return (
            f"[AI Navigator window position: x={geo.x()}, y={geo.y()}, "
            f"width={geo.width()}, height={geo.height()}. "
            f"If this window covers important content, tell the user to minimize or move it — NOT close it.]"
        )

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
        self._main_window.message_submitted.connect(self._on_user_message)
        self._floating_window.message_submitted.connect(self._on_user_message)

        # Engine → UI
        self._engine.response_ready.connect(self._on_response_ready)
        self._engine.error_occurred.connect(self._on_error)
        self._engine.processing_started.connect(self._on_processing_started)
        self._engine.processing_finished.connect(self._on_processing_finished)
        self._engine.streaming_chunk.connect(self._main_window.append_streaming_chunk)

        # Screen monitor → engine
        self._engine.screen_monitor.on_change(self._engine.handle_screen_change)

        # Floating window actions
        self._floating_window.correction_requested.connect(self._on_correction)
        self._floating_window.pause_toggled.connect(self._on_pause_toggle)
        self._floating_window.next_step_requested.connect(self._on_next_step)

        # Session management
        self._main_window.new_session_requested.connect(self._on_new_session)
        self._main_window.save_session_requested.connect(self._on_save_session)

    @Slot(str)
    def _on_user_message(self, text: str) -> None:
        """Handle user message from any input source."""
        self._main_window.add_message("user", text)
        self._engine.handle_user_message(text)

    @Slot()
    def _on_processing_started(self) -> None:
        self._main_window.set_processing(True)
        self._streaming_active = True
        self._main_window.begin_streaming_message()

    @Slot()
    def _on_processing_finished(self) -> None:
        self._main_window.set_processing(False)
        if getattr(self, "_streaming_active", False):
            self._main_window.end_streaming_message()
            self._streaming_active = False

    @Slot(object)
    def _on_response_ready(self, sequencer: StepSequencer) -> None:
        """Handle AI response — show overlay and update chat."""
        step = sequencer.current_step
        if step is None:
            return

        # Always write the final instruction via add_message — streaming is
        # visual-only feedback and is removed by end_streaming_message before this.
        self._main_window.add_message("assistant", step.instruction)

        # Show progress
        progress = sequencer.get_progress()
        if progress:
            self._main_window.show_status(progress)

        # Copy to clipboard if needed
        if step.clipboard:
            copy_to_clipboard(step.clipboard)
            self._main_window.add_message(
                "system", f"Copied to clipboard: {step.clipboard}"
            )

        # Locate element and show overlay
        if step.target_text:
            result = self._engine.element_locator.locate(
                target_text=step.target_text,
                target_role=step.target_role.value if step.target_role else None,
                target_region=step.target_region.value if step.target_region else None,
            )

            if result.bbox:
                self._overlay.show_overlay(
                    bbox=result.bbox,
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
            self._main_window.update_token_display(session.total_tokens)

    @Slot(str)
    def _on_error(self, message: str) -> None:
        """Handle errors from the engine."""
        self._main_window.add_message("system", f"Error: {message}")
        self._main_window.show_status("Error occurred")
        logger.error("Engine error: %s", message)

    @Slot()
    def _on_correction(self) -> None:
        """Handle correction request."""
        self._main_window.add_message("system", "Correction requested — re-analyzing...")
        self._overlay.clear()
        self._engine.handle_correction()

    @Slot()
    def _on_pause_toggle(self) -> None:
        """Handle pause toggle."""
        paused = self._engine.toggle_pause()
        self._floating_window.set_paused(paused)
        self._main_window.show_status("Paused" if paused else "Active")
        if paused:
            self._overlay.clear()

    @Slot()
    def _on_next_step(self) -> None:
        """Handle next step request."""
        self._overlay.clear()
        self._engine.handle_next_step()

    @Slot()
    def _on_new_session(self) -> None:
        """Handle new session request."""
        self._main_window.clear_chat()
        self._overlay.clear()
        self._engine.step_sequencer.reset()
        self._main_window.show_status("New session — type your request")
        self._main_window.add_message(
            "system",
            "New session started. What would you like help with?"
        )

    @Slot()
    def _on_save_session(self) -> None:
        """Handle save session request."""
        session = self._engine.session_manager.current_session
        if session:
            path = self._engine.session_manager.save_session(session)
            self._main_window.show_status(f"Session saved: {path.name}")
        else:
            self._main_window.show_status("No active session to save")

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
            self._main_window.add_message("system", f"Warning: {hint}")
        else:
            self._main_window.add_message(
                "system",
                f"AI Navigator ready — using {router.provider_name}\n"
                "Type a task description to get started.\n"
                "Example: 'Help me buy a USB-C cable on Amazon'"
            )

        # Start engine
        self._engine.start()

        # Start screen monitor polling
        self._monitor_timer.start()

        # Show windows
        self._main_window.show()

        # Run Qt event loop
        try:
            exit_code = self._app.exec()
        finally:
            self._monitor_timer.stop()
            self._engine.stop()

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
