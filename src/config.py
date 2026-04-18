"""Configuration management for AI Navigator.

Loads settings from .env file and provides typed access to all configuration values.
Uses pydantic-settings for validation and environment variable loading.
"""

import logging
from functools import lru_cache
from pathlib import Path
from typing import Optional

from pydantic import Field
from pydantic_settings import BaseSettings


class Config(BaseSettings):
    """Application configuration loaded from environment variables and .env file."""

    model_config = {"env_file": ".env", "env_file_encoding": "utf-8", "extra": "ignore"}

    # --- API ---
    api_provider: str = Field(
        default="anthropic",
        description="AI backend: anthropic | gemini | ollama | openai",
    )

    # Anthropic
    anthropic_api_key: Optional[str] = Field(default=None, alias="ANTHROPIC_API_KEY")
    anthropic_model: str = Field(
        default="claude-sonnet-4-6",
        description="Anthropic model ID. Options: claude-haiku-4-5-20251001 (fast/cheap), "
                    "claude-sonnet-4-6 (balanced), claude-opus-4-6 (most capable)",
    )
    anthropic_fast_model: str = Field(
        default="claude-haiku-4-5-20251001",
        description="Faster/cheaper model for screen-change re-queries. "
                    "Set to same as anthropic_model to disable tiering.",
    )

    # Google Gemini — free tier via Google AI Studio (no credit card)
    gemini_api_key: Optional[str] = Field(default=None, alias="GEMINI_API_KEY")
    gemini_model: str = Field(
        default="gemini-2.5-flash",
        description=(
            "Gemini model ID. "
            "gemini-2.5-flash: default, free tier, strong vision, fast. "
            "gemini-2.5-flash-lite: cheapest paid option ($0.10/MTok), good for re-queries. "
            "gemini-2.5-pro: highest quality, free tier, use for initial analysis. "
            "gemini-3.1-flash-lite-preview: newest free option, worth benchmarking."
        ),
    )
    gemini_fast_model: str = Field(
        default="gemini-2.5-flash-lite",
        description=(
            "Cheaper Gemini model for automated screen-change re-queries. "
            "gemini-2.5-flash-lite: $0.10/MTok input, free tier available. "
            "Set to same as gemini_model to disable tiering."
        ),
    )

    # Ollama — local inference, no API key, runs on-device
    ollama_base_url: str = Field(
        default="http://localhost:11434",
        description="Ollama server URL. Default: http://localhost:11434",
    )
    ollama_model: str = Field(
        default="llama3.2-vision",
        description="Ollama model name. Vision-capable models: llama3.2-vision, llava:7b, moondream. "
                    "Pull with: ollama pull llama3.2-vision",
    )
    ollama_timeout_sec: int = Field(
        default=120,
        description="Ollama request timeout (longer than cloud APIs — local inference is slower)",
    )

    # OpenAI (stub — basic support, v0.4 full implementation)
    openai_api_key: Optional[str] = Field(default=None, alias="OPENAI_API_KEY")
    openai_model: str = Field(
        default="gpt-4o",
        description="OpenAI model ID. Options: gpt-4o (default), gpt-4o-mini, gpt-4-turbo",
    )

    # Shared API settings
    api_timeout_sec: int = Field(default=30, description="Cloud API request timeout in seconds")
    api_max_retries: int = Field(default=3, description="Max retry attempts for API calls")

    # --- Logging ---
    log_level: str = Field(default="INFO")
    debug_mode: bool = Field(default=False)

    # --- Screen Capture ---
    capture_interval_ms: int = Field(default=2000, description="Idle fallback capture interval")
    max_screenshot_width: int = Field(default=1920)
    max_screenshot_height: int = Field(default=1080)
    # API-send image is downscaled separately from the local OCR capture.
    # 768×432 = 2 Gemini/Claude tiles (~3,200 tokens vs 12,800 at 1920×1080 — 75% reduction).
    max_api_screenshot_width: int = Field(default=768, description="Max width sent to AI API for normal requests (token optimization)")
    max_api_screenshot_height: int = Field(default=432, description="Max height sent to AI API for normal requests (token optimization)")
    max_api_full_screenshot_width: int = Field(default=1280, description="Max width for force_full requests (Start Menu, taskbar, system dialogs). Higher than normal cap since full-desktop context matters more than token savings in these rare cases.")
    max_api_full_screenshot_height: int = Field(default=720, description="Max height for force_full requests.")
    enable_active_window_crop: bool = Field(
        default=True,
        description="Crop API screenshot to the foreground window before sending (reduces tokens ~80%)",
    )
    diff_thumbnail_width: int = Field(default=160, description="Low-res thumbnail for pixel-diff")
    diff_thumbnail_height: int = Field(default=90)
    diff_fps: int = Field(default=10, description="Pixel-diff check frequency")
    diff_threshold: float = Field(default=0.05, description="Pixel change threshold (0-1)")
    phash_threshold: int = Field(default=5, description="pHash Hamming distance threshold")
    idle_timeout_sec: int = Field(default=10, description="Seconds before idle fallback check")
    checkpoint_auto_advance: bool = Field(
        default=False,
        description=(
            "Auto-complete checkpoint steps when a large screen change is detected "
            "(e.g. page navigation). When False (default), every step requires the "
            "→ Next button — the AI only re-queries when you ask. Enable for fully "
            "guided walkthroughs where you want continuous step-by-step guidance."
        ),
    )
    checkpoint_auto_advance_threshold: float = Field(
        default=0.30,
        description=(
            "Fraction of pixels that must change to auto-complete a checkpoint step "
            "(0.0–1.0). Only used when checkpoint_auto_advance=True. "
            "0.30 catches full page navigations; raise to require a bigger change."
        ),
    )

    # --- OCR ---
    ocr_confidence_threshold: float = Field(default=0.5, description="Minimum OCR confidence")
    ocr_lang: str = Field(default="en", description="OCR language")

    # --- Element Locator ---
    enable_a11y: bool = Field(default=True, description="Use OS Accessibility API as primary locator")
    enable_ocr: bool = Field(default=True, description="Use PaddleOCR as fallback locator")
    a11y_timeout_ms: int = Field(default=100, description="Max time for A11y query in ms")

    # --- Overlay ---
    overlay_color: str = Field(default="#FF6B35", description="Overlay highlight color")
    overlay_arrow_color: str = Field(default="#FF6B35", description="Arrow color")
    overlay_thickness: int = Field(default=4, description="Overlay inner stroke thickness (white outline is 2x+2)")
    subtitle_font_size: int = Field(default=18, description="Subtitle text font size")
    subtitle_bg_opacity: int = Field(default=180, description="Subtitle background opacity (0-255)")
    subtitle_duration_sec: int = Field(
        default=0,
        description="Subtitle display duration in seconds. 0 = auto (persists until next instruction).",
    )

    # --- Token Budget ---
    daily_token_cap: int = Field(default=100_000, description="Daily token cap")
    monthly_token_cap: int = Field(default=5_000_000, description="Monthly token cap")
    cost_safety_margin: float = Field(default=2.5, description="Cost estimate multiplier (reduce as optimizations mature)")

    # --- Hotkeys ---
    correction_hotkey: str = Field(default="alt+e", description="Trigger re-analysis (Alt+E = rEtry/Error)")
    pause_hotkey: str = Field(default="alt+s", description="Pause/resume screen capture (Alt+S = Stop/Start)")
    next_step_hotkey: str = Field(default="alt+`", description="Advance to next step (Alt+` = leftmost key, easy pinky)")
    floating_window_hotkey: str = Field(default="alt+q", description="Toggle floating panel (Alt+Q = Quit/show)")
    talk_hotkey: str = Field(default="alt+a", description="Push-to-talk voice input (Alt+A = Ask/Audio)")
    reread_hotkey: str = Field(default="alt+r", description="Re-read last instruction via TTS (Alt+R = Read/Replay)")

    # --- Paths ---
    session_dir: Path = Field(
        default_factory=lambda: Path.home() / ".ai-navigator" / "sessions",
        description="Session storage directory",
    )
    token_usage_file: Path = Field(
        default_factory=lambda: Path.home() / ".ai-navigator" / "token_usage.json",
        description="Token usage tracking file",
    )

    # --- Feature Flags ---
    enable_tts: bool = Field(default=False, description="Text-to-speech output via pyttsx3")
    tts_rate: int = Field(default=175, description="TTS speech rate (words per minute)")
    tts_volume: float = Field(default=1.0, description="TTS volume (0.0-1.0)")
    enable_voice_input: bool = Field(default=False, description="Voice input via microphone")
    capture_indicator_visible: bool = Field(default=True, description="Show capture active indicator")

    def ensure_dirs(self) -> None:
        """Create required directories if they don't exist."""
        self.session_dir.mkdir(parents=True, exist_ok=True)
        self.token_usage_file.parent.mkdir(parents=True, exist_ok=True)


@lru_cache(maxsize=1)
def get_config() -> Config:
    """Get the cached configuration singleton."""
    config = Config()
    config.ensure_dirs()
    return config


def setup_logging(config: Optional[Config] = None) -> None:
    """Configure logging based on config settings."""
    if config is None:
        config = get_config()
    level = getattr(logging, config.log_level.upper(), logging.INFO)
    logging.basicConfig(
        level=level,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )
    if config.debug_mode:
        logging.getLogger("ai_navigator").setLevel(logging.DEBUG)
