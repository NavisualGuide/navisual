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

    # Google Gemini — free tier via Google AI Studio (~1,500 req/day, no credit card)
    gemini_api_key: Optional[str] = Field(default=None, alias="GEMINI_API_KEY")
    gemini_model: str = Field(
        default="gemini-2.0-flash",
        description="Gemini model ID. Options: gemini-2.0-flash (free tier), "
                    "gemini-1.5-pro (higher quality, paid)",
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

    # OpenAI (v0.2)
    openai_api_key: Optional[str] = Field(default=None, alias="OPENAI_API_KEY")

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
    diff_thumbnail_width: int = Field(default=160, description="Low-res thumbnail for pixel-diff")
    diff_thumbnail_height: int = Field(default=90)
    diff_fps: int = Field(default=10, description="Pixel-diff check frequency")
    diff_threshold: float = Field(default=0.05, description="Pixel change threshold (0-1)")
    phash_threshold: int = Field(default=5, description="pHash Hamming distance threshold")
    idle_timeout_sec: int = Field(default=10, description="Seconds before idle fallback check")

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

    # --- Token Budget ---
    daily_token_cap: int = Field(default=100_000, description="Daily token cap")
    monthly_token_cap: int = Field(default=5_000_000, description="Monthly token cap")
    cost_safety_margin: float = Field(default=2.5, description="Cost estimate multiplier (reduce as optimizations mature)")

    # --- Hotkeys ---
    correction_hotkey: str = Field(default="ctrl+shift+x", description="Trigger re-analysis")
    pause_hotkey: str = Field(default="ctrl+shift+p", description="Pause/resume screen capture")
    next_step_hotkey: str = Field(default="ctrl+shift+n", description="Advance to next step")
    floating_window_hotkey: str = Field(default="ctrl+shift+space", description="Toggle floating window")

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
    enable_tts: bool = Field(default=False, description="Text-to-speech (v0.2)")
    enable_voice_input: bool = Field(default=False, description="Voice input (v0.2)")
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
